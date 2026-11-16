//! 1.2/S2: bounded in-memory ledger for caller-supplied `client_request_id`
//! deduplication and result query.
//!
//! The ledger records request ids -> {state, result summary, timestamps} for
//! `herdr_call` executions. A repeated `client_request_id` returns the recorded
//! result without re-executing. Records are intentionally in-memory only (the
//! design doc forbids a state.db schema change for S2); a restart therefore
//! forgets pending entries and a post-restart query correctly reports
//! `unknown`.
//!
//! The ledger lives inside `StateStore` so the existing `Arc<Mutex<StateStore>>`
//! transport-agnostic context reaches it. Locking order: callers always take
//! the store lock; the ledger itself adds no separate lock.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum distinct live entries; oldest entries are evicted beyond this.
pub const REQUEST_LEDGER_MAX_ENTRIES: usize = 1024;
/// Entries expire 30 minutes after their last update.
pub const REQUEST_LEDGER_TTL_MS: i64 = 30 * 60 * 1000;
/// `client_request_id` validation mirrors `idempotency_key` hygiene.
pub const CLIENT_REQUEST_ID_MAX_LEN: usize = 256;
/// Stored results are capped so a prompt-sized payload never lands verbatim in
/// the ledger; larger results keep a digest plus head/tail summary only.
const RESULT_SUMMARY_MAX_BYTES: usize = 8192;

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Monotonic, process-unique runtime request id. `req_` prefix keeps it
/// visually distinct from wire-level link request ids.
pub fn next_request_id() -> String {
    let seq = REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req_{}_{}", std::process::id(), seq)
}

/// Validation rules match `idempotency_key`: non-empty, length-capped, no
/// leading/trailing whitespace, no control characters.
pub fn validate_client_request_id(value: &Value) -> Result<String, Value> {
    let Some(key) = value.as_str() else {
        return Err(json!({
            "ok": false,
            "code": "invalid_params",
            "retryable": false,
            "delivery_state": "not_delivered",
            "field": "client_request_id",
            "message": "client_request_id must be a string",
        }));
    };
    if key.is_empty() {
        return Err(json!({
            "ok": false,
            "code": "invalid_params",
            "retryable": false,
            "delivery_state": "not_delivered",
            "field": "client_request_id",
            "message": "client_request_id must be a non-empty string",
        }));
    }
    if key.chars().count() > CLIENT_REQUEST_ID_MAX_LEN {
        return Err(json!({
            "ok": false,
            "code": "invalid_params",
            "retryable": false,
            "delivery_state": "not_delivered",
            "field": "client_request_id",
            "message": format!("client_request_id exceeds {CLIENT_REQUEST_ID_MAX_LEN} characters"),
        }));
    }
    if key != key.trim() || key.chars().any(|c| c.is_control()) {
        return Err(json!({
            "ok": false,
            "code": "invalid_params",
            "retryable": false,
            "delivery_state": "not_delivered",
            "field": "client_request_id",
            "message": "client_request_id must not have leading/trailing whitespace or control characters",
        }));
    }
    Ok(key.to_owned())
}

/// Terminal/in-flight classification for `herdr_mcp.request.status`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestLedgerState {
    Pending,
    Applied,
    Rejected,
    NotDelivered,
    Unknown,
}

impl RequestLedgerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::NotDelivered => "not_delivered",
            Self::Unknown => "unknown",
        }
    }
}

/// Map an execution result to a ledger state.
///
/// - `ok:true` -> `applied`.
/// - `ok:false` with `delivery_state == "not_delivered"` -> `not_delivered`.
/// - `ok:false` with `delivery_state == "delivery_unknown"`, or a retryable
///   timeout-shaped failure -> `unknown` (the write may have been applied).
/// - every other `ok:false` -> `rejected` (admission/schema/policy failure;
///   the underlying call never performed its mutation).
pub fn classify_result_state(result: &Value) -> RequestLedgerState {
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return RequestLedgerState::Applied;
    }
    match result.get("delivery_state").and_then(Value::as_str) {
        Some("not_delivered") => return RequestLedgerState::NotDelivered,
        Some("delivery_unknown") => return RequestLedgerState::Unknown,
        _ => {}
    }
    // A timeout/cancel mid-flight is exactly the "maybe applied" class; the
    // code names are the ones the link and local transport already emit.
    if matches!(
        result.get("code").and_then(Value::as_str),
        Some("request_timeout") | Some("local_mcp_timeout") | Some("delivery_uncertain")
    ) {
        return RequestLedgerState::Unknown;
    }
    RequestLedgerState::Rejected
}

/// A stored result. Results larger than the summary cap are replaced by a
/// bounded projection; the digest still lets a caller correlate the full
/// out-of-band response.
#[derive(Clone, Debug)]
enum StoredResult {
    Full(Value),
    Summarized {
        code: Option<String>,
        ok: bool,
        sha256_hex: String,
        truncated_bytes: usize,
    },
}

fn store_result(result: &Value) -> StoredResult {
    let serialized = serde_json::to_string(result).unwrap_or_default();
    if serialized.len() <= RESULT_SUMMARY_MAX_BYTES {
        return StoredResult::Full(result.clone());
    }
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    StoredResult::Summarized {
        code: result
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ok: result.get("ok").and_then(Value::as_bool).unwrap_or(false),
        sha256_hex: format!("{:016x}", hasher.finish()),
        truncated_bytes: serialized.len() - RESULT_SUMMARY_MAX_BYTES,
    }
}

#[derive(Clone, Debug)]
pub struct RequestLedgerRecord {
    pub request_id: String,
    pub client_request_id: String,
    pub method: String,
    pub state: RequestLedgerState,
    result: StoredResult,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl RequestLedgerRecord {
    /// Result projection for a replay or a `request.status` answer.
    fn result_json(&self) -> Value {
        match &self.result {
            StoredResult::Full(result) => result.clone(),
            StoredResult::Summarized {
                code,
                ok,
                sha256_hex,
                truncated_bytes,
            } => json!({
                "ok": ok,
                "code": code.clone().unwrap_or_else(|| "result_summarized".to_owned()),
                "result_digest": sha256_hex,
                "result_truncated_bytes": truncated_bytes,
                "message": "stored result exceeded the ledger summary cap; the digest identifies the full response",
            }),
        }
    }
    fn status_json(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "client_request_id": self.client_request_id,
            "method": self.method,
            "state": self.state.as_str(),
            "result": self.result_json(),
            "created_ms": self.created_ms,
            "updated_ms": self.updated_ms,
            "ttl_ms": REQUEST_LEDGER_TTL_MS,
        })
    }
}

/// Lookup outcome that already carries the public response shape.
pub enum LedgerReplay {
    /// A terminal record exists; replay its stored result.
    Final(RequestLedgerRecord),
    /// A record exists but the first submission is still executing.
    InFlight(RequestLedgerRecord),
    /// Nothing recorded for this key.
    Miss,
}
/// Outcome of an atomic dedupe + reserve under one lock.
pub enum LedgerBegin {
    /// New key: execute and then `settle` with the result.
    Execute { request_id: String },
    /// Terminal record exists: replay it, never re-execute.
    Replay(RequestLedgerRecord),
    /// First submission still executing: fail closed.
    InFlight(RequestLedgerRecord),
}

#[derive(Default, Debug)]
pub struct RequestLedger {
    entries: HashMap<String, RequestLedgerRecord>,
}

impl RequestLedger {
    fn evict_expired(&mut self, now_ms: i64) {
        self.entries
            .retain(|_, record| now_ms - record.updated_ms < REQUEST_LEDGER_TTL_MS);
    }

    /// Dedupe check before execution. Expired entries are dropped first so a
    /// resubmission past the TTL behaves like a fresh request.
    pub fn lookup(&mut self, client_request_id: &str, now_ms: i64) -> LedgerReplay {
        self.evict_expired(now_ms);
        match self.entries.get(client_request_id) {
            Some(record) if record.state == RequestLedgerState::Pending => {
                LedgerReplay::InFlight(record.clone())
            }
            Some(record) => LedgerReplay::Final(record.clone()),
            None => LedgerReplay::Miss,
        }
    }

    /// Reserve a pending record; returns the request id used. When the wire
    /// boundary already minted a correlation id (1.2/S0 `request_id` header or
    /// JSON-RPC id) the ledger adopts it, so `request.status` answers the same
    /// id the caller saw on the envelope; direct `tool_call` paths fall back to
    /// the generated `req_*` id.
    pub fn begin(
        &mut self,
        client_request_id: &str,
        method: &str,
        preferred_request_id: Option<&str>,
        now_ms: i64,
    ) -> String {
        self.evict_expired(now_ms);
        // Capacity: oldest updated entry goes first.
        while self.entries.len() >= REQUEST_LEDGER_MAX_ENTRIES {
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, record)| record.updated_ms)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest_key);
        }
        let request_id = preferred_request_id
            .filter(|id| !id.is_empty() && id.chars().count() <= CLIENT_REQUEST_ID_MAX_LEN)
            .map(str::to_owned)
            .unwrap_or_else(next_request_id);
        self.entries.insert(
            client_request_id.to_owned(),
            RequestLedgerRecord {
                request_id: request_id.clone(),
                client_request_id: client_request_id.to_owned(),
                method: method.to_owned(),
                state: RequestLedgerState::Pending,
                result: StoredResult::Full(Value::Null),
                created_ms: now_ms,
                updated_ms: now_ms,
            },
        );
        request_id
    }

    /// Settle a pending record with the execution result.
    pub fn settle(&mut self, client_request_id: &str, result: &Value, now_ms: i64) {
        if let Some(record) = self.entries.get_mut(client_request_id)
            && record.state == RequestLedgerState::Pending
        {
            record.state = classify_result_state(result);
            record.result = store_result(result);
            record.updated_ms = now_ms;
        }
    }

    /// Public query used by `herdr_mcp.request.status`.
    pub fn status(&mut self, key: &str, now_ms: i64) -> Value {
        match self.lookup(key, now_ms) {
            LedgerReplay::Final(record) | LedgerReplay::InFlight(record) => {
                let mut status = record.status_json();
                status["ok"] = json!(true);
                status["found"] = json!(true);
                status
            }
            LedgerReplay::Miss => {
                if let Some(record) = self.by_request_id(key) {
                    let mut status = record.status_json();
                    status["ok"] = json!(true);
                    status["found"] = json!(true);
                    return status;
                }
                json!({
                    "ok": true,
                    "found": false,
                    "state": "unknown",
                    "message": "no ledger record for this request id; it expired, predates this runtime generation, or was never submitted with client_request_id",
                })
            }
        }
    }

    /// Atomic dedupe + reserve: one store lock covers the lookup and the
    /// pending insert, so two concurrent submissions of the same key can
    /// never both execute.
    pub fn begin_or_replay(
        &mut self,
        client_request_id: &str,
        method: &str,
        preferred_request_id: Option<&str>,
        now_ms: i64,
    ) -> LedgerBegin {
        match self.lookup(client_request_id, now_ms) {
            LedgerReplay::Final(record) => LedgerBegin::Replay(record),
            LedgerReplay::InFlight(record) => LedgerBegin::InFlight(record),
            LedgerReplay::Miss => LedgerBegin::Execute {
                request_id: self.begin(client_request_id, method, preferred_request_id, now_ms),
            },
        }
    }

    /// Find a record by its runtime-generated `request_id` (linear scan; the
    /// ledger is small and status queries are infrequent).
    pub fn by_request_id(&self, request_id: &str) -> Option<&RequestLedgerRecord> {
        self.entries
            .values()
            .find(|record| record.request_id == request_id)
    }
}

/// Public response for a duplicate `client_request_id` while the first call is
/// still executing: fail closed, never double-execute.
pub fn in_flight_response(record: &RequestLedgerRecord) -> Value {
    json!({
        "ok": false,
        "code": "request_in_flight",
        "retryable": false,
        "retry_safe": false,
        "delivery_state": "delivery_unknown",
        "request_id": record.request_id,
        "client_request_id": record.client_request_id,
        "message": "a request with this client_request_id is still executing; query herdr_mcp.request.status for the outcome instead of resubmitting",
        "status_hint": {"method": "herdr_mcp.request.status", "params": {"client_request_id": record.client_request_id}},
    })
}

/// Public response for a terminal replay: the recorded result plus replay
/// markers, never a second execution.
pub fn replay_response(record: &RequestLedgerRecord) -> Value {
    let mut result = record.result_json();
    if let Some(object) = result.as_object_mut() {
        object.insert("request_id".to_owned(), json!(record.request_id));
        object.insert(
            "client_request_id".to_owned(),
            json!(record.client_request_id),
        );
        object.insert("deduplicated".to_owned(), json!(true));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_result() -> Value {
        json!({"ok": true, "task_id": "t1"})
    }

    #[test]
    fn duplicate_submission_returns_recorded_result_without_reexecuting() {
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "agent.dispatch", None, 1000);
        ledger.settle("c1", &ok_result(), 1005);
        match ledger.lookup("c1", 1010) {
            LedgerReplay::Final(record) => {
                let replay = replay_response(&record);
                assert_eq!(replay["task_id"], "t1");
                assert_eq!(replay["deduplicated"], true);
            }
            _ => panic!("expected terminal replay"),
        }
    }

    #[test]
    fn in_flight_duplicate_does_not_double_execute() {
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "agent.dispatch", None, 1000);
        match ledger.lookup("c1", 1001) {
            LedgerReplay::InFlight(record) => {
                let response = in_flight_response(&record);
                assert_eq!(response["code"], "request_in_flight");
                assert_eq!(response["delivery_state"], "delivery_unknown");
            }
            _ => panic!("expected in-flight"),
        }
    }

    #[test]
    fn rejection_and_not_delivered_states_are_distinguished() {
        assert_eq!(
            classify_result_state(&json!({"ok": false, "code": "invalid_params"})),
            RequestLedgerState::Rejected
        );
        assert_eq!(
            classify_result_state(&json!({"ok": false, "delivery_state": "not_delivered"})),
            RequestLedgerState::NotDelivered
        );
        assert_eq!(
            classify_result_state(&json!({"ok": false, "code": "request_timeout"})),
            RequestLedgerState::Unknown
        );
    }

    #[test]
    fn ttl_expiry_makes_resubmission_fresh() {
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "agent.dispatch", None, 1000);
        ledger.settle("c1", &ok_result(), 1001);
        match ledger.lookup("c1", 1001 + REQUEST_LEDGER_TTL_MS + 1) {
            LedgerReplay::Miss => {}
            _ => panic!("expired entry must not replay"),
        }
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut ledger = RequestLedger::default();
        for index in 0..REQUEST_LEDGER_MAX_ENTRIES {
            let key = format!("c{index}");
            ledger.begin(&key, "m", None, index as i64);
            ledger.settle(&key, &ok_result(), index as i64);
        }
        ledger.begin("overflow", "m", None, REQUEST_LEDGER_MAX_ENTRIES as i64);
        assert!(matches!(ledger.lookup("c0", 1_000_000), LedgerReplay::Miss));
        assert!(matches!(
            ledger.lookup("overflow", 1_000_000),
            LedgerReplay::InFlight(_)
        ));
    }

    #[test]
    fn status_reports_unknown_for_missing_key() {
        let mut ledger = RequestLedger::default();
        let status = ledger.status("nope", 1000);
        assert_eq!(status["state"], "unknown");
        assert_eq!(status["found"], false);
    }

    #[test]
    fn client_request_id_hygiene_matches_idempotency_key() {
        assert!(validate_client_request_id(&json!("abc-123")).is_ok());
        for bad in [
            json!(""),
            json!("  pad"),
            json!("tab\tinside"),
            json!("x".repeat(CLIENT_REQUEST_ID_MAX_LEN + 1)),
            json!(42),
        ] {
            let err = validate_client_request_id(&bad).unwrap_err();
            assert_eq!(err["code"], "invalid_params");
            assert_eq!(err["delivery_state"], "not_delivered");
        }
    }

    #[test]
    fn oversized_result_is_summarized_not_stored_verbatim() {
        let huge = json!({"ok": true, "blob": "x".repeat(RESULT_SUMMARY_MAX_BYTES + 100)});
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "m", None, 0);
        ledger.settle("c1", &huge, 1);
        let status = ledger.status("c1", 2);
        assert!(status["result"]["result_digest"].is_string());
        assert!(status["result"].get("blob").is_none());
    }

    #[test]
    fn wire_request_id_is_adopted_over_generated() {
        let mut ledger = RequestLedger::default();
        let id = ledger.begin("c1", "m", Some("edge-req-7"), 0);
        assert_eq!(id, "edge-req-7");
        ledger.settle("c1", &ok_result(), 1);
        // Status resolves by the wire id the caller saw on the envelope.
        let status = ledger.status("edge-req-7", 2);
        assert_eq!(status["found"], true);
        assert_eq!(status["client_request_id"], "c1");
    }
}
