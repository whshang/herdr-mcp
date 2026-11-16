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

/// A stored result is never the verbatim response: the ledger only keeps a
/// bounded projection (status fields + digest + byte count) so prompt text,
/// file content, or any other payload body can never be read back through
/// `request.status`, regardless of result size.
#[derive(Clone, Debug)]
enum StoredResult {
    /// No result yet (pending record).
    None,
    Summarized {
        code: Option<String>,
        ok: bool,
        digest_hex: String,
        result_bytes: usize,
    },
}

fn store_result(result: &Value) -> StoredResult {
    let serialized = serde_json::to_string(result).unwrap_or_default();
    StoredResult::Summarized {
        code: result
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ok: result.get("ok").and_then(Value::as_bool).unwrap_or(false),
        digest_hex: digest_hex(&serialized),
        result_bytes: serialized.len(),
    }
}

/// Bounded caller/request identity recorded at `begin`: a fingerprint of the
/// request payload (never the payload itself) plus the caller's scope, so a
/// same-key replay is only honored for the same request from the same scope.
#[derive(Clone, Debug)]
pub struct LedgerIdentity {
    /// Digest of the canonicalized `{method, params}` the key was submitted
    /// with. Two different requests sharing one key are a conflict, never a
    /// replay of whichever landed first.
    pub request_digest: String,
    /// Minimal caller-scope fingerprint (session/device); `request.status`
    /// and replays honor a record only for the scope that created it.
    pub caller_scope: String,
}

impl LedgerIdentity {
    /// Digest a canonical request pair. serde_json object serialization is
    /// already key-sorted for `Map`, so this is stable across encoders.
    pub fn of(method: &str, params: &Value, caller_scope: &str) -> Self {
        Self {
            request_digest: digest_hex(&json!({"method": method, "params": params}).to_string()),
            caller_scope: caller_scope.to_owned(),
        }
    }
}

fn digest_hex(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Clone, Debug)]
pub struct RequestLedgerRecord {
    pub request_id: String,
    pub client_request_id: String,
    pub method: String,
    /// Fingerprint of the request the key was first seen with.
    request_digest: String,
    /// Scope that may replay or query this record.
    caller_scope: String,
    pub state: RequestLedgerState,
    result: StoredResult,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl RequestLedgerRecord {
    /// Result projection for a replay or a `request.status` answer. Only the
    /// bounded projection ever leaves the ledger: `ok`, the failure code, a
    /// content digest, and the original byte size — never the payload itself.
    fn result_json(&self) -> Value {
        match &self.result {
            StoredResult::None => Value::Null,
            StoredResult::Summarized {
                code,
                ok,
                digest_hex,
                result_bytes,
            } => json!({
                "ok": ok,
                "code": code.clone().unwrap_or_else(|| "result_recorded".to_owned()),
                "result_digest": digest_hex,
                "result_bytes": result_bytes,
                "result_summary": true,
                "message": "the ledger stores a digest-only projection; it never retains the response body",
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
    /// Nothing recorded for this key (or the record belongs to another
    /// caller scope, which is reported identically so key existence never
    /// leaks across scopes).
    Miss,
}
/// Outcome of an atomic dedupe + reserve under one lock.
pub enum LedgerBegin {
    /// New key: execute and then `settle` with the result.
    Execute { request_id: String },
    /// Terminal record exists with the same request fingerprint and scope:
    /// replay it, never re-execute.
    Replay(RequestLedgerRecord),
    /// First submission still executing: fail closed.
    InFlight(RequestLedgerRecord),
    /// Same key but a different request fingerprint or caller scope: the
    /// caller must pick a new `client_request_id`, never silently replay.
    Conflict,
    /// At capacity with no terminal entry evictable: refuse the new key
    /// rather than dropping an in-flight record.
    CapacityExceeded,
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
    /// resubmission past the TTL behaves like a fresh request. `caller_scope`
    /// must match the recorded scope: another scope sees `Miss`, never the
    /// record, so existence never leaks across scopes.
    pub fn lookup(
        &mut self,
        client_request_id: &str,
        caller_scope: &str,
        now_ms: i64,
    ) -> LedgerReplay {
        self.evict_expired(now_ms);
        match self.entries.get(client_request_id) {
            Some(record) if record.caller_scope == caller_scope => {
                if record.state == RequestLedgerState::Pending {
                    LedgerReplay::InFlight(record.clone())
                } else {
                    LedgerReplay::Final(record.clone())
                }
            }
            Some(_) | None => LedgerReplay::Miss,
        }
    }

    /// Reserve a pending record; returns the request id used. When the wire
    /// boundary already minted a correlation id (1.2/S0 `request_id` header or
    /// JSON-RPC id) the ledger adopts it, so `request.status` answers the same
    /// id the caller saw on the envelope; direct `tool_call` paths fall back to
    /// the generated `req_*` id.
    ///
    /// Capacity eviction is terminal-only: an in-flight record must never be
    /// dropped, because dropping it would let a duplicate of an in-flight
    /// mutation execute twice. With no terminal entry evictable the new key is
    /// refused instead.
    pub fn begin(
        &mut self,
        client_request_id: &str,
        method: &str,
        identity: &LedgerIdentity,
        preferred_request_id: Option<&str>,
        now_ms: i64,
    ) -> Result<String, Value> {
        self.evict_expired(now_ms);
        while self.entries.len() >= REQUEST_LEDGER_MAX_ENTRIES {
            // Only terminal entries are evictable.
            let Some(oldest_key) = self
                .entries
                .iter()
                .filter(|(_, record)| record.state != RequestLedgerState::Pending)
                .min_by_key(|(_, record)| record.updated_ms)
                .map(|(key, _)| key.clone())
            else {
                return Err(json!({
                    "ok": false,
                    "code": "request_ledger_full",
                    "retryable": true,
                    "delivery_state": "not_delivered",
                    "message": "request ledger is at capacity with in-flight work; retry after the in-flight requests settle",
                }));
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
                request_digest: identity.request_digest.clone(),
                caller_scope: identity.caller_scope.clone(),
                state: RequestLedgerState::Pending,
                result: StoredResult::None,
                created_ms: now_ms,
                updated_ms: now_ms,
            },
        );
        Ok(request_id)
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

    /// Public query used by `herdr_mcp.request.status`. `caller_scope` must
    /// match the record's scope; a foreign-scope record answers `found:false`
    /// exactly like an unknown key, so existence never leaks across scopes.
    pub fn status(&mut self, key: &str, caller_scope: &str, now_ms: i64) -> Value {
        match self.lookup(key, caller_scope, now_ms) {
            LedgerReplay::Final(record) | LedgerReplay::InFlight(record) => {
                let mut status = record.status_json();
                status["ok"] = json!(true);
                status["found"] = json!(true);
                status
            }
            LedgerReplay::Miss => {
                if let Some(record) = self.by_request_id(key)
                    && record.caller_scope == caller_scope
                {
                    let mut status = record.status_json();
                    status["ok"] = json!(true);
                    status["found"] = json!(true);
                    return status;
                }
                json!({
                    "ok": true,
                    "found": false,
                    "state": "unknown",
                    "message": "no ledger record for this request id; it expired, predates this runtime generation, was never submitted with client_request_id, or belongs to a different caller scope",
                })
            }
        }
    }

    /// Atomic dedupe + reserve: one store lock covers the lookup and the
    /// pending insert, so two concurrent submissions of the same key can
    /// never both execute. A hit requires the same request fingerprint and
    /// the same caller scope; anything else under the same key is a conflict,
    /// never a silent replay of a different request.
    pub fn begin_or_replay(
        &mut self,
        client_request_id: &str,
        method: &str,
        identity: &LedgerIdentity,
        preferred_request_id: Option<&str>,
        now_ms: i64,
    ) -> LedgerBegin {
        self.evict_expired(now_ms);
        match self.entries.get(client_request_id) {
            Some(record)
                if record.request_digest == identity.request_digest
                    && record.caller_scope == identity.caller_scope =>
            {
                if record.state == RequestLedgerState::Pending {
                    LedgerBegin::InFlight(record.clone())
                } else {
                    LedgerBegin::Replay(record.clone())
                }
            }
            // Same key, different request or caller: conflict, not replay and
            // not a fresh execution over the existing record.
            Some(_) => LedgerBegin::Conflict,
            None => match self.begin(
                client_request_id,
                method,
                identity,
                preferred_request_id,
                now_ms,
            ) {
                Ok(request_id) => LedgerBegin::Execute { request_id },
                Err(_) => LedgerBegin::CapacityExceeded,
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

    fn identity() -> LedgerIdentity {
        LedgerIdentity::of("m", &json!({}), "scope-a")
    }

    #[test]
    fn duplicate_submission_returns_recorded_result_without_reexecuting() {
        let mut ledger = RequestLedger::default();
        ledger
            .begin("c1", "agent.dispatch", &identity(), None, 1000)
            .unwrap();
        ledger.settle("c1", &ok_result(), 1005);
        match ledger.lookup("c1", "scope-a", 1010) {
            LedgerReplay::Final(record) => {
                let replay = replay_response(&record);
                // Digest-only projection: replays never re-emit the payload.
                assert_eq!(replay["result_summary"], true);
                assert!(replay["result_digest"].is_string());
                assert!(replay.get("task_id").is_none());
                assert_eq!(replay["deduplicated"], true);
            }
            _ => panic!("expected terminal replay"),
        }
    }

    #[test]
    fn in_flight_duplicate_does_not_double_execute() {
        let mut ledger = RequestLedger::default();
        ledger
            .begin("c1", "agent.dispatch", &identity(), None, 1000)
            .unwrap();
        match ledger.lookup("c1", "scope-a", 1001) {
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
        ledger
            .begin("c1", "agent.dispatch", &identity(), None, 1000)
            .unwrap();
        ledger.settle("c1", &ok_result(), 1001);
        match ledger.lookup("c1", "scope-a", 1001 + REQUEST_LEDGER_TTL_MS + 1) {
            LedgerReplay::Miss => {}
            _ => panic!("expired entry must not replay"),
        }
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut ledger = RequestLedger::default();
        for index in 0..REQUEST_LEDGER_MAX_ENTRIES {
            let key = format!("c{index}");
            ledger
                .begin(&key, "m", &identity(), None, index as i64)
                .unwrap();
            ledger.settle(&key, &ok_result(), index as i64);
        }
        ledger
            .begin(
                "overflow",
                "m",
                &identity(),
                None,
                REQUEST_LEDGER_MAX_ENTRIES as i64,
            )
            .unwrap();
        assert!(matches!(
            ledger.lookup("c0", "scope-a", 1_000_000),
            LedgerReplay::Miss
        ));
        assert!(matches!(
            ledger.lookup("overflow", "scope-a", 1_000_000),
            LedgerReplay::InFlight(_)
        ));
    }

    #[test]
    fn status_reports_unknown_for_missing_key() {
        let mut ledger = RequestLedger::default();
        let status = ledger.status("nope", "scope-a", 1000);
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

    /// user caller | Given a small successful result | When it settles in the
    /// ledger | Then `request.status` and replay expose only the digest
    /// projection — the response body is never stored or readable back.
    #[test]
    fn small_result_is_digest_only_never_verbatim() {
        let result = json!({"ok": true, "text": "secret prompt body with NOVOS_AGENT_TRACE_HUB"});
        let mut ledger = RequestLedger::default();
        ledger
            .begin("c1", "agent.prompt", &identity(), None, 0)
            .unwrap();
        ledger.settle("c1", &result, 1);
        let status = ledger.status("c1", "scope-a", 2);
        assert_eq!(status["found"], true);
        assert_eq!(status["result"]["ok"], true);
        assert_eq!(status["result"]["result_summary"], true);
        assert!(status["result"]["result_digest"].is_string());
        assert_eq!(
            status["result"]["result_bytes"],
            json!(serde_json::to_string(&result).unwrap().len())
        );
        assert!(status["result"].get("text").is_none());
        // A duplicate submission replays the same projection, not the body.
        match ledger.lookup("c1", "scope-a", 3) {
            LedgerReplay::Final(record) => {
                let replay = replay_response(&record);
                assert!(replay.get("text").is_none());
                assert!(replay["result_digest"].is_string());
            }
            _ => panic!("expected terminal replay"),
        }
    }

    #[test]
    fn oversized_result_is_summarized_not_stored_verbatim() {
        let huge = json!({"ok": true, "blob": "x".repeat(8192 + 100)});
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "m", &identity(), None, 0).unwrap();
        ledger.settle("c1", &huge, 1);
        let status = ledger.status("c1", "scope-a", 2);
        assert!(status["result"]["result_digest"].is_string());
        assert!(status["result"].get("blob").is_none());
    }

    /// user caller | Given one key already bound to a request | When the same
    /// key arrives with a different request digest | Then it is a conflict,
    /// never a replay of the first request.
    #[test]
    fn same_key_different_request_is_conflict_not_replay() {
        let mut ledger = RequestLedger::default();
        let identity_a = LedgerIdentity::of("agent.prompt", &json!({"text": "a"}), "scope-a");
        ledger
            .begin("c1", "agent.prompt", &identity_a, None, 0)
            .unwrap();
        ledger.settle("c1", &ok_result(), 1);

        let identity_b = LedgerIdentity::of("agent.prompt", &json!({"text": "b"}), "scope-a");
        assert!(matches!(
            ledger.begin_or_replay("c1", "agent.prompt", &identity_b, None, 2),
            LedgerBegin::Conflict
        ));
        // The original record still replays for the matching fingerprint.
        assert!(matches!(
            ledger.begin_or_replay("c1", "agent.prompt", &identity_a, None, 3),
            LedgerBegin::Replay(_)
        ));
    }

    /// user caller | Given one key bound to scope-a | When scope-b submits the
    /// same key with the identical request | Then it is a conflict and status
    /// from scope-b hides the record entirely.
    #[test]
    fn foreign_scope_conflict_and_status_hides_record() {
        let mut ledger = RequestLedger::default();
        let identity_a = LedgerIdentity::of("agent.prompt", &json!({}), "scope-a");
        ledger
            .begin("c1", "agent.prompt", &identity_a, None, 0)
            .unwrap();
        ledger.settle("c1", &ok_result(), 1);

        // Same key + same request from another scope: conflict, no replay.
        let identity_b = LedgerIdentity::of("agent.prompt", &json!({}), "scope-b");
        assert!(matches!(
            ledger.begin_or_replay("c1", "agent.prompt", &identity_b, None, 2),
            LedgerBegin::Conflict
        ));
        // request.status from scope-b must not observe scope-a's record.
        let foreign = ledger.status("c1", "scope-b", 3);
        assert_eq!(foreign["found"], false);
        assert_eq!(foreign["state"], "unknown");
        // The owning scope still resolves it.
        let owned = ledger.status("c1", "scope-a", 4);
        assert_eq!(owned["found"], true);
        assert_eq!(owned["state"], "applied");
    }

    /// user caller | Given a ledger full of pending records | When a new key
    /// arrives | Then it is refused rather than evicting an in-flight record,
    /// so a duplicate of in-flight work can never double-execute.
    #[test]
    fn full_ledger_of_pending_refuses_instead_of_evicting_in_flight() {
        let mut ledger = RequestLedger::default();
        for index in 0..REQUEST_LEDGER_MAX_ENTRIES {
            ledger
                .begin(&format!("c{index}"), "m", &identity(), None, index as i64)
                .unwrap();
        }
        // No terminal entry exists; the new key must be refused.
        assert!(matches!(
            ledger.begin_or_replay("overflow", "m", &identity(), None, 1_000_000),
            LedgerBegin::CapacityExceeded
        ));
        // Once one record settles, that terminal slot becomes evictable.
        ledger.settle("c0", &ok_result(), 1_000_001);
        assert!(matches!(
            ledger.begin_or_replay("overflow", "m", &identity(), None, 1_000_002),
            LedgerBegin::Execute { .. }
        ));
        // Every original pending key is still tracked (in-flight preserved).
        assert!(matches!(
            ledger.lookup("c1", "scope-a", 1_000_003),
            LedgerReplay::InFlight(_)
        ));
    }

    /// user caller | Given a pending record | When a duplicate submission
    /// arrives mid-flight | Then it fails closed with request_in_flight and
    /// the record can still settle once.
    #[test]
    fn settle_after_error_path_leaves_terminal_state() {
        let mut ledger = RequestLedger::default();
        ledger.begin("c1", "m", &identity(), None, 0).unwrap();
        // Simulates the admitted_tool_output error path: settle with
        // delivery_unknown so the key reports unknown, not pending forever.
        ledger.settle(
            "c1",
            &json!({"ok": false, "code": "internal_error", "delivery_state": "delivery_unknown"}),
            1,
        );
        match ledger.lookup("c1", "scope-a", 2) {
            LedgerReplay::Final(record) => {
                assert_eq!(record.state, RequestLedgerState::Unknown);
            }
            _ => panic!("error settle must produce a terminal record"),
        }
        let status = ledger.status("c1", "scope-a", 3);
        assert_eq!(status["state"], "unknown");
        // And the record is terminal, so a same-request resubmit replays it.
        assert!(matches!(
            ledger.begin_or_replay("c1", "m", &identity(), None, 4),
            LedgerBegin::Replay(_)
        ));
    }

    #[test]
    fn wire_request_id_is_adopted_over_generated() {
        let mut ledger = RequestLedger::default();
        let id = ledger
            .begin("c1", "m", &identity(), Some("edge-req-7"), 0)
            .unwrap();
        assert_eq!(id, "edge-req-7");
        ledger.settle("c1", &ok_result(), 1);
        // Status resolves by the wire id the caller saw on the envelope.
        let status = ledger.status("edge-req-7", "scope-a", 2);
        assert_eq!(status["found"], true);
        assert_eq!(status["client_request_id"], "c1");
    }
}
