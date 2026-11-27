//! #332 D-2: durable `client_request_id` idempotency ledger.
//!
//! D-1 (`docs/design/332-durable-idempotency.md`) made the caller-boundary key
//! restart-decidable by persisting it in the existing schema-16 `operations`
//! table under a dedicated operation kind, without touching `SCHEMA_VERSION`
//! or adding columns. The key is global inside one kind: the persisted
//! `idempotency_key` is a domain-separated SHA-256 digest of
//! `(kind, client_request_id)` with no caller scope, so two scopes sharing one
//! key collide on `(kind, idempotency_key)` and produce a single authoritative
//! row. Caller scope and the request fingerprint live inside `request_hash`;
//! any mismatch under the same key is one uniform `idempotency_conflict` that
//! echoes nothing about the existing record.
//!
//! Write-intent rows keep `expires_at IS NULL` from reservation through
//! `complete` and through the hash-only tombstone: the legacy generic prune
//! (`DELETE ... expires_at IS NOT NULL AND expires_at <= now`) can never free
//! a write key on a new or old binary. The 24h full-summary retention is a
//! logical deadline inside `result_json.summary_expires_at_ms`; the kind's own
//! cleanup (inside the reservation transaction) compresses overdue summaries
//! into `expired:true` tombstones. Read-intent rows are the only ones that may
//! carry `expires_at` and be deleted.
//!
//! A `pending` row means the first call is still executing *or* the process
//! died before settling. `boot_id` distinguishes them: same boot →
//! `request_in_flight`; a different boot → the row is converted in place to a
//! durable `unknown` tombstone and answered `delivery_unknown` +
//! `inspect_before_retry`. Neither is ever replayed automatically.

use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::request_ledger::{
    CLIENT_REQUEST_ID_MAX_LEN, REQUEST_LEDGER_TTL_MS, classify_result_state, next_request_id,
};
use crate::state_store::mutation_leases::sha256_hex;
use crate::state_store::{ClientRequestRecord, ClientRequestReservation, StateStore};

/// Durable ledger kind for `herdr_call` `client_request_id` records.
pub const HERDR_CALL_CLIENT_REQUEST_KIND: &str = "herdr_call.client_request.v1";
/// Reserved for the future `herdr_exec` caller-boundary key. Defined so the
/// namespace is fixed now; deliberately not wired into `herdr_exec` — that
/// needs the contract change D-1 deferred to separate review.
pub const HERDR_EXEC_CLIENT_REQUEST_KIND: &str = "herdr_exec.client_request.v1";
/// #1094 P0-a: first-class write tools accept `client_request_id` inside
/// `params.arguments` (the contracts do not declare it — option (b), no
/// epoch/hash change). One kind per tool keeps each key's namespace disjoint
/// from `herdr_call` and from each other.
pub const HERDR_FS_WRITE_CLIENT_REQUEST_KIND: &str = "herdr_fs_write.client_request.v1";
pub const HERDR_FS_EDIT_CLIENT_REQUEST_KIND: &str = "herdr_fs_edit.client_request.v1";
pub const HERDR_FS_PATCH_CLIENT_REQUEST_KIND: &str = "herdr_fs_patch.client_request.v1";
pub const HERDR_PROMPT_CLIENT_REQUEST_KIND: &str = "herdr_prompt.client_request.v1";
/// All durable client-request kinds; shared by cleanup and status scans.
pub const CLIENT_REQUEST_KINDS: [&str; 6] = [
    HERDR_CALL_CLIENT_REQUEST_KIND,
    HERDR_EXEC_CLIENT_REQUEST_KIND,
    HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
    HERDR_FS_EDIT_CLIENT_REQUEST_KIND,
    HERDR_FS_PATCH_CLIENT_REQUEST_KIND,
    HERDR_PROMPT_CLIENT_REQUEST_KIND,
];
/// Logical full-summary retention for settled rows: `result_json` carries the
/// bounded projection for 24h, after which kind-specific cleanup compacts the
/// row into a hash-only tombstone (`expires_at` stays NULL).
pub const CLIENT_REQUEST_SUMMARY_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Maximum durable rows across both client-request kinds. Write-intent
/// pending/unknown/complete/tombstone rows are never evictable; when no
/// expired read-only terminal row frees a slot the reservation is refused
/// instead of dropping a record.
pub const CLIENT_REQUEST_LEDGER_MAX_ROWS: i64 = 1024;

const CLIENT_REQUEST_KEY_DOMAIN: &str = "herdr-mcp/client-request/v2";
const REQUEST_HASH_PREFIX: &str = "v2";

static DURABLE_BOOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Process-unique runtime boot id. `StateStore::open` mints one per open so a
/// reopened store can distinguish "pending from my own boot" (still in flight)
/// from "pending from a dead boot" (delivery unknown). The sequence suffix
/// keeps rapid open/close cycles (tests) unique inside one millisecond.
pub fn new_durable_boot_id() -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    format!(
        "boot_{}_{}_{}",
        std::process::id(),
        now_ms,
        DURABLE_BOOT_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Domain-separated, cross-process-stable key digest. The raw
/// `client_request_id` never reaches the database; `kind` inside the digest
/// gives `herdr_call` and `herdr_exec` disjoint namespaces for the same raw
/// key. Deliberately excludes caller scope: the same key under a different
/// identity must collide, not reserve a second execution.
pub fn client_request_key_digest(kind: &str, client_request_id: &str) -> String {
    sha256_hex(&format!(
        "{CLIENT_REQUEST_KEY_DOMAIN}\0{kind}\0{client_request_id}"
    ))
}

/// Stable `op_id` embedding the kind so two kinds reusing one raw key get
/// independent primary keys. `key_digest[..32]` is 32 hex chars = 16 bytes
/// (~122 bits) of the SHA-256 prefix.
/// #1094 P0-a: the first-class write tools that accept `client_request_id` in
/// `params.arguments`, mapped to their durable-ledger kinds. Every other tool
/// keeps its pre-existing dedupe surface.
pub(crate) fn client_request_tool_kind(name: &str) -> Option<&'static str> {
    match name {
        "herdr_call" => Some(HERDR_CALL_CLIENT_REQUEST_KIND),
        "herdr_exec" => Some(HERDR_EXEC_CLIENT_REQUEST_KIND),
        "herdr_fs_write" => Some(HERDR_FS_WRITE_CLIENT_REQUEST_KIND),
        "herdr_fs_edit" => Some(HERDR_FS_EDIT_CLIENT_REQUEST_KIND),
        "herdr_fs_patch" => Some(HERDR_FS_PATCH_CLIENT_REQUEST_KIND),
        "herdr_prompt" => Some(HERDR_PROMPT_CLIENT_REQUEST_KIND),
        // A caller may echo back the kind string a `matches` entry reported.
        _ => CLIENT_REQUEST_KINDS
            .iter()
            .find(|kind| **kind == name)
            .copied(),
    }
}

/// The narrower set that actually wraps a dispatch in a durable
/// `client_request_id` reservation: `herdr_call` keeps its dedicated earlier
/// path, and `herdr_exec`'s kind is reserved but deliberately *not* wired —
/// mapping the tool name here only decides status lookups and silent-drop
/// checks, never ledger wrapping. Keeping the two maps separate means a
/// `client_request_id` on `herdr_exec` is rejected as unsupported instead of
/// being silently recorded with a hard-coded write intent or sharing two
/// idempotency slots.
pub(crate) fn client_request_dispatch_tool_kind(name: &str) -> Option<&'static str> {
    match name {
        "herdr_fs_write" => Some(HERDR_FS_WRITE_CLIENT_REQUEST_KIND),
        "herdr_fs_edit" => Some(HERDR_FS_EDIT_CLIENT_REQUEST_KIND),
        "herdr_fs_patch" => Some(HERDR_FS_PATCH_CLIENT_REQUEST_KIND),
        "herdr_prompt" => Some(HERDR_PROMPT_CLIENT_REQUEST_KIND),
        _ => None,
    }
}

/// The kind embedded in a client-request `op_id` (`op:{kind}:{digest}`).
/// This relies on kind names never containing `:` — every entry of
/// `CLIENT_REQUEST_KINDS` is a dotted `*.client_request.v1` identifier, so
/// `rsplit_once` splits at the digest boundary, not inside the kind.
fn kind_from_op_id(op_id: &str) -> &str {
    op_id
        .strip_prefix("op:")
        .and_then(|rest| rest.rsplit_once(':'))
        .map(|(kind, _)| kind)
        .unwrap_or("")
}

pub fn client_request_op_id(kind: &str, key_digest: &str) -> String {
    let prefix_len = key_digest.len().min(32);
    format!("op:{kind}:{}", &key_digest[..prefix_len])
}

/// Canonical digest of the `{method, params}` payload. serde_json serializes
/// `Map` keys sorted, so this is stable across encoders and processes.
fn canonical_request_digest(method: &str, params: &Value) -> String {
    sha256_hex(&json!({"method": method, "params": params}).to_string())
}

/// Durable request identity stored in `operations.request_hash`:
/// `"v2:" + sha256(caller scope) + ":" + sha256(canonical {method,params}) +
/// ":" + request_id`. The composite keeps the scope digest extractable for
/// `request.status` scope checks and keeps the first call's `request_id`
/// recoverable for status-by-id and replay echo. Replay/conflict eligibility
/// compares the binding prefix (segments 1..3); the trailing `request_id` is
/// evidence recorded by the first call, not a binding field — a retry mints a
/// new `request_id` and must still replay.
pub fn durable_request_hash(scope: &str, method: &str, params: &Value, request_id: &str) -> String {
    format!(
        "{}:{}:{}:{request_id}",
        REQUEST_HASH_PREFIX,
        sha256_hex(scope),
        canonical_request_digest(method, params)
    )
}

/// `"v2:<scope_digest>:<request_digest>:"` — the binding portion a
/// resubmission must match. `record.request_hash.starts_with(prefix)` is the
/// exact full-hash comparison with the recorded `request_id` retained.
pub fn durable_request_hash_prefix(scope: &str, method: &str, params: &Value) -> String {
    format!(
        "{}:{}:{}:",
        REQUEST_HASH_PREFIX,
        sha256_hex(scope),
        canonical_request_digest(method, params)
    )
}

/// Scope digest recorded inside `request_hash` segment 2, used by
/// `request.status` to hide records from foreign scopes.
pub fn durable_scope_digest(scope: &str) -> String {
    sha256_hex(scope)
}

/// Extract the scope digest (segment 2) of a stored `request_hash`.
pub fn durable_scope_digest_from_hash(request_hash: &str) -> Option<&str> {
    let rest = request_hash.strip_prefix("v2:")?;
    rest.split(':').next()
}

/// Extract the recorded first-call `request_id` (last segment).
pub fn request_id_from_hash(request_hash: &str) -> Option<&str> {
    request_hash.strip_prefix("v2:")?.rsplit(':').next()
}

/// Mint the ledger `request_id`: adopt the wire correlation id when it is
/// ledger-safe (non-empty, length-capped, no `:` so the composite hash stays
/// segment-unambiguous), else a generated `req_*` id — the same rule the
/// in-memory ledger applies in `RequestLedger::begin`.
pub fn preferred_client_request_id(wire_request_id: Option<&str>) -> String {
    wire_request_id
        .filter(|id| {
            !id.is_empty() && !id.contains(':') && id.chars().count() <= CLIENT_REQUEST_ID_MAX_LEN
        })
        .map(str::to_owned)
        .unwrap_or_else(next_request_id)
}

/// Fail-closed read-only classification for `herdr_call` methods. Only
/// methods that are replay-safe *and* provably side-effect free belong here;
/// everything else — including unknown methods — is a write. The
/// classification decides what happens when no durable caller scope is
/// available: writes are refused (`durable_caller_scope_unavailable`), reads
/// may fall back to the existing in-memory ledger.
pub fn is_read_only_client_request_method(method: &str) -> bool {
    use crate::progressive_skills as skills;
    if method.trim().eq_ignore_ascii_case("server.status") {
        return true;
    }
    matches!(
        method,
        skills::REQUEST_STATUS_METHOD
            | skills::LOCAL_LIST_METHOD
            | skills::LOCAL_DESCRIBE_METHOD
            | skills::LOCAL_LOAD_METHOD
            | skills::GITHUB_STATUS_METHOD
            | skills::CLEANUP_PREVIEW_METHOD
            | skills::BROWSER_ENDPOINT_LIST_METHOD
            | skills::BROWSER_ENDPOINT_INSPECT_METHOD
            | skills::BROWSER_RESOURCE_LIST_METHOD
            | skills::BROWSER_RESOURCE_INSPECT_METHOD
            | skills::WORK_MEMORY_SEARCH_METHOD
            | skills::WORK_MEMORY_RESUME_METHOD
            | skills::BILIBILI_VIDEO_TRANSCRIPT_METHOD
            | skills::DOUBAO_IMAGE_STATUS_METHOD
            | skills::X_SEARCH_POSTS_METHOD
            | skills::X_THREAD_READ_METHOD
            | crate::codex_history::LIST_METHOD
            | crate::codex_history::READ_METHOD
            | crate::prompt::AGENT_TASK_STATUS_METHOD
            | crate::prompt::AGENT_TASK_INBOX_METHOD
            | crate::prompt::decision_gates::STATUS
            | skills::TEXT_READ_METHOD
    )
}

/// Derive the restart-stable caller scope for a dispatch, or `None` when no
/// persistent caller identity is resolvable.
///
/// - Browser caller: requires both the session identity and the WebChat
///   authorization; resolves through the durable caller-session tables to a
///   stable `session_ref`. Local-device sessions get the explicit
///   `device_local` marker so `None` and cross-device `Some(device_id)` never
///   collapse into one scope. An unresolvable or unvalidated session yields
///   `None` — never a fallback to the opaque session string, which is not a
///   restart-stable identity.
/// - Non-browser caller: the enrolled `edge_device_id` from local config.
/// - A verified lease holder binds an extra `|lease:<sha256>` segment; it is
///   never an identity on its own and the lease token never enters the scope.
pub fn durable_caller_scope(
    context: &crate::mcp::RuntimeContext<'_>,
    store: &StateStore,
) -> Option<String> {
    let base = if context.caller_browser_session.is_some() {
        match context.caller_webchat_authorization {
            Some(authorization) => {
                let session = context.caller_browser_session?;
                match store.resolve_browser_caller_session_qualified(
                    &authorization.principal_ref,
                    &session.provider,
                    &session.opaque_session_id,
                ) {
                    Ok(Some((session_ref, owning_device_id))) => Some(format!(
                        "browser:{}:{}:{}",
                        session.provider,
                        session_ref,
                        owning_device_id.unwrap_or_else(|| "device_local".to_owned())
                    )),
                    Ok(None) | Err(_) => None,
                }
            }
            // A browser-shaped caller without a resolvable principal has no
            // persistent identity; do not relabel it as a local caller.
            None => None,
        }
    } else {
        context
            .local_device_id
            .map(|device_id| format!("local:{device_id}"))
    };
    base.map(|scope| match context.caller_lease_holder_id {
        Some(holder) if !holder.is_empty() => {
            format!("{scope}|lease:{}", sha256_hex(holder))
        }
        _ => scope,
    })
}

/// Parsed + precomputed durable identity for one `client_request_id`
/// submission. Reused by `begin` and `settle` so both operate on identical
/// fingerprints.
#[derive(Debug, Clone)]
pub struct ClientRequestIds {
    pub kind: String,
    pub key_digest: String,
    pub request_hash: String,
    pub op_id: String,
    pub request_id: String,
    /// Bounded pending payload written at reservation: the recorded
    /// `request_id` for status projection and the intent for foreign-boot
    /// unknown conversion. Never carries params, results, or the raw
    /// `client_request_id` — D-1 forbids the raw key in the database.
    pub reservation_json: String,
}

pub fn client_request_ids(
    kind: &str,
    scope: &str,
    method: &str,
    params: &Value,
    client_request_id: &str,
    request_id: &str,
    read_only: bool,
) -> ClientRequestIds {
    let key_digest = client_request_key_digest(kind, client_request_id);
    ClientRequestIds {
        kind: kind.to_owned(),
        op_id: client_request_op_id(kind, &key_digest),
        request_hash: durable_request_hash(scope, method, params, request_id),
        request_id: request_id.to_owned(),
        key_digest,
        reservation_json: json!({
            "schema": 1,
            "intent": if read_only { "read_only" } else { "mutating" },
            "request_id": request_id,
        })
        .to_string(),
    }
}

/// Outcome of `durable_begin_or_replay` for an admitted submission.
pub enum ClientRequestOutcome {
    /// Fresh reservation: execute, then `durable_settle_result`.
    Execute { request_id: String },
    /// Terminal `complete` record with matching fingerprint: replay the
    /// recorded bounded projection. Body goes through the model-visible
    /// projection like any other replayed result.
    Replay(Value),
    /// Any refusal: respond with the body verbatim. All refusal bodies carry
    /// `ok:false`; the caller derives `is_error` from that.
    Respond(Value),
}

/// The public `request_id` a stored record reports: `result_json.request_id`
/// after settle, else the `request_hash` tail recorded at reservation.
fn record_request_id(record: &ClientRequestRecord) -> Option<String> {
    record
        .result()
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| request_id_from_hash(&record.request_hash).map(str::to_owned))
}

fn conflict_response(client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "idempotency_conflict",
        "retryable": false,
        "delivery_state": "not_delivered",
        "fallback_allowed": false,
        "client_request_id": client_request_id,
        "message": "client_request_id was already used for a different request or caller scope; choose a new key rather than replaying",
    })
}

fn expired_response(client_request_id: &str, op_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "idempotency_key_expired",
        "retryable": false,
        "delivery_state": "not_delivered",
        "fallback_allowed": false,
        "safe_retry_mode": "none",
        "client_request_id": client_request_id,
        "op_id": op_id,
        "message": "the recorded summary for this client_request_id expired; the key stays tombstoned forever — choose a new key to start a new request",
    })
}

fn in_flight_response(record: &ClientRequestRecord, client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "request_in_flight",
        "retryable": false,
        "retry_safe": false,
        "delivery_state": "delivery_unknown",
        "safe_retry_mode": "inspect_before_retry",
        "request_id": record_request_id(record),
        "client_request_id": client_request_id,
        "op_id": record.op_id,
        "message": "a request with this client_request_id is still executing; query herdr_mcp.request.status for the outcome instead of resubmitting",
        "status_hint": {"method": "herdr_mcp.request.status", "params": {"client_request_id": client_request_id}},
    })
}

fn delivery_unknown_response(record: &ClientRequestRecord, client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "delivery_unknown",
        "retryable": false,
        "retry_safe": false,
        "delivery_state": "delivery_unknown",
        "safe_retry_mode": "inspect_before_retry",
        "request_id": record_request_id(record),
        "client_request_id": client_request_id,
        "op_id": record.op_id,
        "state": "unknown",
        "message": "the first attempt may have been delivered before the runtime restarted; inspect the target state and retry only with a new client_request_id",
        "status_hint": {"method": "herdr_mcp.request.status", "params": {"client_request_id": client_request_id}},
    })
}

fn store_unavailable_response(client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "durable_store_unavailable",
        "retryable": true,
        "delivery_state": "not_delivered",
        "fallback_allowed": false,
        "safe_retry_mode": "retry_after_durable_store_recovery",
        "client_request_id": client_request_id,
        "message": "the durable request ledger is unreachable; the request was not executed — retry after the store recovers",
    })
}

fn ledger_full_response(client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "client_request_ledger_full",
        "retryable": true,
        "delivery_state": "not_delivered",
        "fallback_allowed": false,
        "client_request_id": client_request_id,
        "message": "the durable client-request ledger is at capacity with no expired read-only records to evict; pending and unknown records are never dropped",
    })
}

/// Public refusal when a write-intent method carries no resolvable durable
/// caller scope: nothing is reserved and nothing executes.
pub fn durable_scope_unavailable_response(client_request_id: &str) -> Value {
    json!({
        "ok": false,
        "code": "durable_caller_scope_unavailable",
        "retryable": false,
        "delivery_state": "not_delivered",
        "fallback_allowed": false,
        "client_request_id": client_request_id,
        "message": "client_request_id requires a persistent caller identity (bound browser caller session or enrolled edge_device_id); no durable ledger row was written and nothing was executed",
    })
}

/// Project a stored terminal `result_json` into the public replay response:
/// the recorded bounded summary plus replay markers — never a second
/// execution and never the stored scope digest.
fn replay_response(record: &ClientRequestRecord, client_request_id: &str) -> Value {
    let result = record.result();
    let summary = result.get("result_summary").cloned().unwrap_or(Value::Null);
    let mut response = json!({
        "ok": summary.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "code": summary.get("code").cloned().unwrap_or_else(|| json!("result_recorded")),
        "result_summary": summary,
        "safe_retry_mode": result.get("safe_retry_mode").cloned().unwrap_or_else(|| json!("none")),
        "idempotency_persisted": true,
        "deduplicated": true,
        "op_id": record.op_id,
        "request_id": record_request_id(record),
        "client_request_id": client_request_id,
        "message": "the ledger stores a digest-only projection plus whitelisted receipt identifiers; it never retains the response body",
    });
    // Surface the whitelisted receipt identifiers the first response carried
    // (task_id/receipt_id for herdr_prompt) so a replayed submission returns
    // the same receipt rather than only a digest.
    if let Some(receipt) = result.get("receipt").and_then(Value::as_object) {
        for (field, value) in receipt {
            if !value.is_null() {
                response[field] = value.clone();
            }
        }
    }
    response
}

/// Atomic dedupe + reserve for one durable submission. `Execute` must be
/// followed by `durable_settle_result`; every other outcome is an already
/// shaped public response (`Replay` for the stored projection, `Respond` for
/// conflict/in-flight/unknown/expired/store refusals).
///
/// Retry policy inside is deliberately bounded: a foreign-boot `pending` row
/// is converted to `unknown` in place; if the row raced past `pending` (a
/// live writer settled meanwhile — only possible under a split-brain open)
/// the loop re-reads once instead of guessing.
#[allow(clippy::too_many_arguments)]
pub fn durable_begin_or_replay(
    store: &mut StateStore,
    ids: &ClientRequestIds,
    client_request_id: &str,
    expected_scope: &str,
    method: &str,
    params: &Value,
    read_only: bool,
    now_ms: i64,
) -> ClientRequestOutcome {
    let boot_id = store.durable_boot_id().to_owned();
    let expected_prefix = durable_request_hash_prefix(expected_scope, method, params);
    let expires_at = read_only.then(|| now_ms + REQUEST_LEDGER_TTL_MS);
    for _ in 0..4 {
        match store.reserve_client_request(
            &ids.kind,
            &ids.key_digest,
            &ids.request_hash,
            &ids.op_id,
            &ids.reservation_json,
            &boot_id,
            now_ms,
            expires_at,
        ) {
            Err(_) => {
                return ClientRequestOutcome::Respond(store_unavailable_response(
                    client_request_id,
                ));
            }
            Ok(ClientRequestReservation::CapacityExhausted) => {
                return ClientRequestOutcome::Respond(ledger_full_response(client_request_id));
            }
            Ok(ClientRequestReservation::Reserved) => {
                return ClientRequestOutcome::Execute {
                    request_id: ids.request_id.clone(),
                };
            }
            Ok(ClientRequestReservation::Existing(record)) => {
                // Binding check: identical scope digest + canonical request
                // digest. The stored hash tail is the first call's
                // request_id, so a same-request retry still matches. Any
                // mismatch is one uniform conflict — no scope echo.
                if !record.request_hash.starts_with(&expected_prefix) {
                    return ClientRequestOutcome::Respond(conflict_response(client_request_id));
                }
                match record.state.as_deref() {
                    Some("pending") => {
                        if record.boot_id.as_deref() == Some(boot_id.as_str()) {
                            return ClientRequestOutcome::Respond(in_flight_response(
                                &record,
                                client_request_id,
                            ));
                        }
                        // Pending written by a dead boot: delivery was never
                        // confirmed. Convert to a durable unknown tombstone;
                        // never re-execute.
                        match store.settle_pending_client_request_unknown(
                            &ids.kind,
                            &ids.key_digest,
                            now_ms,
                        ) {
                            Ok(Some(unknown_record)) => {
                                return ClientRequestOutcome::Respond(delivery_unknown_response(
                                    &unknown_record,
                                    client_request_id,
                                ));
                            }
                            Ok(None) => continue,
                            Err(_) => {
                                return ClientRequestOutcome::Respond(store_unavailable_response(
                                    client_request_id,
                                ));
                            }
                        }
                    }
                    Some("unknown") => {
                        return ClientRequestOutcome::Respond(delivery_unknown_response(
                            &record,
                            client_request_id,
                        ));
                    }
                    Some("complete") => {
                        if record.result().get("expired").and_then(Value::as_bool) == Some(true) {
                            return ClientRequestOutcome::Respond(expired_response(
                                client_request_id,
                                &record.op_id,
                            ));
                        }
                        return ClientRequestOutcome::Replay(replay_response(
                            &record,
                            client_request_id,
                        ));
                    }
                    // Any unexpected stored state fails closed as unknown:
                    // never re-execute under a key whose first outcome the
                    // runtime cannot prove.
                    _ => {
                        return ClientRequestOutcome::Respond(delivery_unknown_response(
                            &record,
                            client_request_id,
                        ));
                    }
                }
            }
        }
    }
    ClientRequestOutcome::Respond(store_unavailable_response(client_request_id))
}

/// How a just-executed call was persisted; the caller uses it to annotate the
/// returned result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableSettle {
    /// `settled/complete` with `outcome:applied` + `safe_retry_mode:none`.
    Completed,
    /// Rejected/not-delivered: the reservation row was atomically released —
    /// no terminal record exists, so a later same-key resubmission reserves
    /// fresh after its resource checks pass again.
    Released,
    /// `settled/unknown` tombstone with `inspect_before_retry`.
    RecordedUnknown,
    /// The terminal write itself failed (e.g. the SQLite commit errored):
    /// the outcome may be committed while the row still reads as pending,
    /// so a same-key resubmission pins as in-flight until restart decays it.
    /// Callers must inspect live state before any retry.
    PersistFailed,
}

/// Bounded `result_json` for a settled record: status fields, digest and byte
/// count only — never response body, stdout, prompt, tokens, file content,
/// or the raw `client_request_id` (D-1 keeps the raw key out of the DB).
/// `receipt` carries only small identifier fields a retry legitimately needs
/// back (`task_id`/`receipt_id` for `herdr_prompt` submissions); it is a
/// whitelist, not the result body, so payload content still never lands in
/// the projection.
fn settled_result_json(
    read_only: bool,
    outcome: &str,
    delivery_state: &str,
    safe_retry_mode: &str,
    request_id: &str,
    result: &Value,
    settled_ms: i64,
) -> String {
    let serialized = serde_json::to_string(result).unwrap_or_default();
    json!({
        "schema": 1,
        "intent": if read_only { "read_only" } else { "mutating" },
        "outcome": outcome,
        "summary_expires_at_ms": settled_ms + CLIENT_REQUEST_SUMMARY_TTL_MS,
        "delivery_state": delivery_state,
        "safe_retry_mode": safe_retry_mode,
        "idempotency_persisted": true,
        "request_id": request_id,
        "result_summary": {
            "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "code": result.get("code").cloned().unwrap_or(Value::Null),
            "result_digest": sha256_hex(&serialized),
            "result_bytes": serialized.len(),
        },
        "receipt": {
            "task_id": result.get("task_id").cloned().unwrap_or(Value::Null),
            "receipt_id": result.get("receipt_id").cloned().unwrap_or(Value::Null),
        },
    })
    .to_string()
}

/// Persist the terminal outcome of an executed reservation using the same
/// `classify_result_state` vocabulary as the in-memory ledger:
///
/// - `Applied` → `settled/complete`, `outcome:applied`, `none`. Read rows get
///   `expires_at = settled + 30min`; write rows stay `NULL` forever.
/// - `Rejected` / `NotDelivered` → the reservation is atomically released; no
///   terminal row is left. The caller still returns the original result and
///   annotates `safe_retry_mode` (`retry_after_resource_release` when the
///   result was retryable, else `none`).
/// - `Unknown` → `settled/unknown` tombstone + `inspect_before_retry`.
///
/// Settlement failures do not mask the execution result: the response to the
/// caller is the real output regardless of whether its projection landed.
pub fn durable_settle_result(
    store: &mut StateStore,
    ids: &ClientRequestIds,
    result: &Value,
    read_only: bool,
    now_ms: i64,
) -> DurableSettle {
    match classify_result_state(result) {
        crate::request_ledger::RequestLedgerState::Applied => {
            let payload = settled_result_json(
                read_only,
                "applied",
                "delivered",
                "none",
                &ids.request_id,
                result,
                now_ms,
            );
            let expires_at = read_only.then(|| now_ms + REQUEST_LEDGER_TTL_MS);
            if store
                .complete_client_request(
                    &ids.kind,
                    &ids.key_digest,
                    &ids.request_hash,
                    &payload,
                    now_ms,
                    expires_at,
                )
                .is_err()
            {
                return DurableSettle::PersistFailed;
            }
            DurableSettle::Completed
        }
        crate::request_ledger::RequestLedgerState::Unknown => {
            let payload = settled_result_json(
                read_only,
                "unknown",
                "delivery_unknown",
                "inspect_before_retry",
                &ids.request_id,
                result,
                now_ms,
            );
            if store
                .complete_client_request_unknown(
                    &ids.kind,
                    &ids.key_digest,
                    &ids.request_hash,
                    &payload,
                    now_ms,
                )
                .is_err()
            {
                return DurableSettle::PersistFailed;
            }
            DurableSettle::RecordedUnknown
        }
        crate::request_ledger::RequestLedgerState::Rejected
        | crate::request_ledger::RequestLedgerState::NotDelivered => {
            if store
                .release_client_request_reservation(
                    &ids.kind,
                    &ids.key_digest,
                    &ids.request_hash,
                    &ids.op_id,
                )
                .is_err()
            {
                // A failed release leaves the pending row pinned exactly
                // like a failed terminal write: callers must inspect.
                return DurableSettle::PersistFailed;
            }
            DurableSettle::Released
        }
        crate::request_ledger::RequestLedgerState::Pending => DurableSettle::Released,
    }
}

/// `request.status` over the durable ledger, checked before the in-memory
/// fallback. `key` is a `client_request_id` or a runtime `request_id`. A
/// record only answers for the scope digest embedded in its `request_hash`;
/// foreign-scope hits return `None` exactly like a miss so existence never
/// leaks across scopes.
pub fn durable_client_request_status(
    store: &StateStore,
    key: &str,
    scope: &str,
    tool: Option<&str>,
) -> Option<Value> {
    let scope_digest = durable_scope_digest(scope);
    // Select by caller scope first: the same key under a different kind or
    // the same raw id submitted by another caller must not leak a foreign
    // record's state, and two kinds may legitimately hold rows for one key.
    // An explicit `tool` that maps to no ledger kind filters to nothing —
    // it must not silently widen back to "all kinds".
    let kind_filter = tool.map(client_request_tool_kind);
    let kind_matches = |kind: &str| match &kind_filter {
        None => true,
        Some(Some(expected)) => kind == *expected,
        Some(None) => false,
    };
    let mut matches = CLIENT_REQUEST_KINDS
        .iter()
        .filter(|kind| kind_matches(kind))
        .filter_map(|kind| {
            store
                .client_request_record(kind, &client_request_key_digest(kind, key))
                .ok()
                .flatten()
        })
        .collect::<Vec<_>>();
    for record in store
        .client_request_records_for_request_id(key)
        .ok()
        .unwrap_or_default()
    {
        if kind_matches(kind_from_op_id(&record.op_id))
            && !matches.iter().any(|seen| seen.op_id == record.op_id)
        {
            matches.push(record);
        }
    }
    let matches = matches
        .into_iter()
        .filter(|record| {
            durable_scope_digest_from_hash(&record.request_hash) == Some(scope_digest.as_str())
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return None;
    }
    // One caller key may legitimately name several tools (an fs_write and a
    // prompt retry can share a client_request_id). Guessing by recency would
    // silently answer for the wrong request, so report the ambiguity and let
    // the caller pass `tool` to disambiguate.
    if matches.len() > 1 {
        return Some(json!({
            "ok": true,
            "found": true,
            "durable": true,
            "ambiguous": true,
            "matches": matches.iter().map(|record| {
                let kind = kind_from_op_id(&record.op_id);
                json!({
                    "kind": kind,
                    // The `tool` value a caller passes to disambiguate: the
                    // tool name is the kind minus its `.client_request.v1`
                    // suffix (`herdr_fs_write.client_request.v1` →
                    // `herdr_fs_write`).
                    "tool": kind.strip_suffix(".client_request.v1").unwrap_or(kind),
                    "op_id": record.op_id,
                    "request_id": record_request_id(record),
                    "state": record.state,
                    "created_ms": record.created_at,
                    "updated_ms": record.updated_at,
                })
            }).collect::<Vec<_>>(),
            "message": "client_request_id maps to several tools; pass the tool name to disambiguate",
        }));
    }
    let record = matches.into_iter().next()?;
    let result = record.result();
    Some(json!({
        "ok": true,
        "found": true,
        "durable": true,
        "op_id": record.op_id,
        "request_id": record_request_id(&record),
        "state": record.state,
        "phase": record.phase,
        "safe_retry_mode": result.get("safe_retry_mode").cloned().unwrap_or_else(|| {
            json!(if record.state.as_deref() == Some("complete") { "none" } else { "inspect_before_retry" })
        }),
        "delivery_state": result.get("delivery_state").cloned().unwrap_or(Value::Null),
        "expired": result.get("expired").and_then(Value::as_bool).unwrap_or(false),
        "result": result.get("result_summary").cloned().unwrap_or(Value::Null),
        "receipt": result.get("receipt").cloned().unwrap_or(Value::Null),
        "created_ms": record.created_at,
        "updated_ms": record.updated_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_state_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-durable-ledger-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn open_store(dir: &std::path::Path) -> StateStore {
        StateStore::open_in_dir(dir, "state").unwrap()
    }

    const KIND: &str = HERDR_CALL_CLIENT_REQUEST_KIND;
    const EXEC_KIND: &str = HERDR_EXEC_CLIENT_REQUEST_KIND;
    const SCOPE_A: &str = "local:device-a";
    const SCOPE_B: &str = "local:device-b";
    const METHOD: &str = "work_memory.bind";
    const KEY: &str = "cr-key-1";

    fn ids_for(scope: &str, params: &Value, request_id: &str, read_only: bool) -> ClientRequestIds {
        client_request_ids(KIND, scope, METHOD, params, KEY, request_id, read_only)
    }

    fn begin(
        store: &mut StateStore,
        ids: &ClientRequestIds,
        scope: &str,
        params: &Value,
        read_only: bool,
        now_ms: i64,
    ) -> ClientRequestOutcome {
        durable_begin_or_replay(store, ids, KEY, scope, METHOD, params, read_only, now_ms)
    }

    fn expect_execute(outcome: ClientRequestOutcome) {
        match outcome {
            ClientRequestOutcome::Execute { .. } => {}
            ClientRequestOutcome::Replay(value) => panic!("expected Execute, got replay {value}"),
            ClientRequestOutcome::Respond(value) => panic!("expected Execute, got {value}"),
        }
    }

    fn respond_body(outcome: ClientRequestOutcome) -> Value {
        match outcome {
            ClientRequestOutcome::Respond(value) => value,
            ClientRequestOutcome::Execute { .. } => panic!("expected Respond, got Execute"),
            ClientRequestOutcome::Replay(value) => panic!("expected Respond, got {value}"),
        }
    }

    fn replay_body(outcome: ClientRequestOutcome) -> Value {
        match outcome {
            ClientRequestOutcome::Replay(value) => value,
            ClientRequestOutcome::Execute { .. } => panic!("expected Replay, got Execute"),
            ClientRequestOutcome::Respond(value) => panic!("expected Replay, got {value}"),
        }
    }

    fn ok_result() -> Value {
        json!({"ok": true, "code": "ok", "bound": true})
    }

    /// user caller | Given a settled client request | When the state store is
    /// reopened and the same key retries | Then the recorded first result
    /// replays instead of executing again.
    #[test]
    fn user_retry_given_settled_write_when_store_reopened_then_first_summary_replays() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        {
            let mut store = open_store(&dir);
            let ids = ids_for(SCOPE_A, &params, "req_first_1", false);
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
            assert_eq!(
                durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100),
                DurableSettle::Completed
            );
        }
        // Reopen = restart: a fresh boot id sees the settled row.
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_second_9", false);
        let replay = replay_body(begin(&mut store, &ids, SCOPE_A, &params, false, 2_000));
        assert_eq!(replay["deduplicated"], true, "{replay}");
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(replay["request_id"], "req_first_1", "{replay}");
        assert_eq!(replay["safe_retry_mode"], "none", "{replay}");
        assert_eq!(replay["result_summary"]["ok"], true, "{replay}");
        assert!(
            replay["op_id"]
                .as_str()
                .unwrap()
                .starts_with("op:herdr_call.client_request.v1:"),
            "{replay}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a settled key under scope A | When scope B reuses
    /// the key | Then one uniform conflict answers and request.status hides
    /// the record from scope B.
    #[test]
    fn user_retry_given_other_scope_same_key_when_retried_then_idempotency_conflict() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_a_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);

        let foreign = ids_for(SCOPE_B, &params, "req_b_1", false);
        let body = respond_body(begin(&mut store, &foreign, SCOPE_B, &params, false, 2_000));
        assert_eq!(body["code"], "idempotency_conflict", "{body}");
        assert_eq!(body["retryable"], false, "{body}");
        assert_eq!(body["delivery_state"], "not_delivered", "{body}");
        assert_eq!(body["fallback_allowed"], false, "{body}");
        let text = body.to_string();
        for leaked in [
            "device-a",
            "device-b",
            "result_summary",
            "op:herdr_call",
            "session_ref",
        ] {
            assert!(!text.contains(leaked), "conflict echoes {leaked}: {text}");
        }

        // Same key + changed params under the original scope also conflicts.
        let changed = ids_for(SCOPE_A, &json!({"other": true}), "req_a_2", false);
        let body = respond_body(begin(
            &mut store,
            &changed,
            SCOPE_A,
            &json!({"other": true}),
            false,
            2_100,
        ));
        assert_eq!(body["code"], "idempotency_conflict", "{body}");

        // Cross-scope status never proves key existence.
        assert!(durable_client_request_status(&store, KEY, SCOPE_B, None).is_none());
        let status = durable_client_request_status(&store, KEY, SCOPE_A, None).unwrap();
        assert_eq!(status["found"], true, "{status}");
        assert_eq!(status["request_id"], "req_a_1", "{status}");
        let by_id = durable_client_request_status(&store, "req_a_1", SCOPE_A, None).unwrap();
        assert_eq!(by_id["found"], true, "{by_id}");
        assert!(durable_client_request_status(&store, "req_a_1", SCOPE_B, None).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given herdr_call and herdr_exec sharing one raw key |
    /// When both reserve | Then two independent rows coexist without a
    /// primary-key clash and replay separately.
    #[test]
    fn user_submit_given_two_kinds_sharing_raw_key_when_reserved_then_both_coexist() {
        let dir = temp_state_dir();
        let params = json!({"m": 1});
        let mut store = open_store(&dir);
        let call_ids = ids_for(SCOPE_A, &params, "req_c_1", false);
        let exec_ids =
            client_request_ids(EXEC_KIND, SCOPE_A, METHOD, &params, KEY, "req_e_1", false);
        assert_ne!(call_ids.op_id, exec_ids.op_id);
        assert_ne!(call_ids.key_digest, exec_ids.key_digest);

        expect_execute(begin(&mut store, &call_ids, SCOPE_A, &params, false, 1_000));
        let exec_outcome = durable_begin_or_replay(
            &mut store, &exec_ids, KEY, SCOPE_A, METHOD, &params, false, 1_010,
        );
        expect_execute(exec_outcome);
        durable_settle_result(&mut store, &call_ids, &ok_result(), false, 1_100);
        durable_settle_result(&mut store, &exec_ids, &ok_result(), false, 1_110);
        let replay = replay_body(begin(&mut store, &call_ids, SCOPE_A, &params, false, 2_000));
        assert_eq!(replay["deduplicated"], true, "{replay}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a settled write row | When the legacy generic
    /// prune deletes expired rows | Then the write row survives with
    /// `expires_at IS NULL` and still replays.
    #[test]
    fn user_write_row_given_settled_when_legacy_prune_runs_then_survives() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_w_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);

        // Exact old-binary prune statement: only NULL expires_at rows may die.
        store
            .execute(
                "DELETE FROM operations
                 WHERE expires_at IS NOT NULL AND expires_at <= 999999999999999",
            )
            .unwrap();
        let record = store
            .client_request_record(KIND, &ids.key_digest)
            .unwrap()
            .expect("write row pruned");
        assert_eq!(record.state.as_deref(), Some("complete"));
        assert_eq!(record.expires_at, None);
        let replay = replay_body(begin(&mut store, &ids, SCOPE_A, &params, false, 2_000));
        assert_eq!(replay["deduplicated"], true, "{replay}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a pending row written before a crash | When the
    /// reopened runtime sees the same key | Then the row becomes a durable
    /// unknown tombstone answered `delivery_unknown` + `inspect_before_retry`.
    #[test]
    fn user_retry_given_pending_from_dead_boot_when_reopened_then_delivery_unknown() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let key_digest;
        {
            let mut store = open_store(&dir);
            let ids = ids_for(SCOPE_A, &params, "req_p_1", false);
            key_digest = ids.key_digest.clone();
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
            // Same-boot resubmission reports in-flight, not unknown.
            let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, 1_050));
            assert_eq!(body["code"], "request_in_flight", "{body}");
            // No settle: the process "dies" with the row pending.
        }
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_p_2", false);
        let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, 2_000));
        assert_eq!(body["code"], "delivery_unknown", "{body}");
        assert_eq!(body["safe_retry_mode"], "inspect_before_retry", "{body}");
        assert_eq!(body["delivery_state"], "delivery_unknown", "{body}");

        let record = store
            .client_request_record(KIND, &key_digest)
            .unwrap()
            .unwrap();
        assert_eq!(record.state.as_deref(), Some("unknown"));
        assert_eq!(record.phase.as_deref(), Some("settled"));
        assert_eq!(record.expires_at, None);
        // The tombstone is terminal: the same key stays unknown, forever.
        let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, 3_000));
        assert_eq!(body["code"], "delivery_unknown", "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a pre-delivery rejection or not_delivered result |
    /// When the reservation settles | Then the row is released and the same
    /// key may reserve again.
    #[test]
    fn user_submit_given_pre_delivery_failure_when_settled_then_reservation_released() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut store = open_store(&dir);
        for result in [
            json!({"ok": false, "code": "single_writer_lease_conflict", "retryable": true}),
            json!({"ok": false, "code": "workspace_busy", "retryable": false, "delivery_state": "not_delivered"}),
        ] {
            let ids = ids_for(SCOPE_A, &params, "req_r_1", false);
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
            assert_eq!(
                durable_settle_result(&mut store, &ids, &result, false, 1_100),
                DurableSettle::Released
            );
            assert!(
                store
                    .client_request_record(KIND, &ids.key_digest)
                    .unwrap()
                    .is_none(),
                "released row must not persist: {result}"
            );
            // The released key reserves cleanly again.
            let ids = ids_for(SCOPE_A, &params, "req_r_2", false);
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_200));
            durable_settle_result(&mut store, &ids, &result, false, 1_300);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a delivery-uncertain result | When the reservation
    /// settles | Then a durable unknown row remains and later retries answer
    /// `delivery_unknown` without re-executing.
    #[test]
    fn user_submit_given_uncertain_result_when_settled_then_unknown_tombstone() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_u_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        assert_eq!(
            durable_settle_result(
                &mut store,
                &ids,
                &json!({"ok": false, "code": "request_timeout", "retryable": true}),
                false,
                1_100,
            ),
            DurableSettle::RecordedUnknown
        );
        let record = store
            .client_request_record(KIND, &ids.key_digest)
            .unwrap()
            .unwrap();
        assert_eq!(record.state.as_deref(), Some("unknown"));
        assert_eq!(record.expires_at, None);
        let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, 2_000));
        assert_eq!(body["code"], "delivery_unknown", "{body}");
        assert_eq!(body["safe_retry_mode"], "inspect_before_retry", "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a settled write summary past its 24h deadline |
    /// When kind cleanup runs | Then the row compacts to a hash-only
    /// tombstone, `expires_at` stays NULL, and the key answers
    /// `idempotency_key_expired`.
    #[test]
    fn user_retry_given_expired_summary_when_cleanup_runs_then_idempotency_key_expired() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_t_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);

        // Past the logical deadline the next reservation compacts the row.
        let past = 1_100 + CLIENT_REQUEST_SUMMARY_TTL_MS + 1;
        let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, past));
        assert_eq!(body["code"], "idempotency_key_expired", "{body}");
        assert_eq!(body["retryable"], false, "{body}");
        assert_eq!(body["fallback_allowed"], false, "{body}");

        let record = store
            .client_request_record(KIND, &ids.key_digest)
            .unwrap()
            .unwrap();
        assert_eq!(record.state.as_deref(), Some("complete"));
        assert_eq!(record.expires_at, None, "tombstone must stay unprunable");
        let result = record.result();
        assert_eq!(result["expired"], true, "{result}");
        assert_eq!(result["outcome"], "expired", "{result}");
        assert!(
            result.get("result_summary").is_none(),
            "tombstone drops the summary: {result}"
        );
        // And still does not die under the legacy prune.
        store
            .execute(
                "DELETE FROM operations
                 WHERE expires_at IS NOT NULL AND expires_at <= 999999999999999",
            )
            .unwrap();
        assert!(
            store
                .client_request_record(KIND, &ids.key_digest)
                .unwrap()
                .is_some()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a read-only request | When it settles | Then the
    /// row carries a 30-minute expiry, is prunable, and expiry frees the key
    /// for a fresh read.
    #[test]
    fn user_read_given_read_only_settled_when_ttl_passes_then_pruned_and_rereserved() {
        let dir = temp_state_dir();
        let params = json!({});
        let mut store = open_store(&dir);
        let ids = client_request_ids(
            KIND,
            SCOPE_A,
            crate::progressive_skills::REQUEST_STATUS_METHOD,
            &params,
            KEY,
            "req_ro_1",
            true,
        );
        let outcome = durable_begin_or_replay(
            &mut store,
            &ids,
            KEY,
            SCOPE_A,
            crate::progressive_skills::REQUEST_STATUS_METHOD,
            &params,
            true,
            1_000,
        );
        expect_execute(outcome);
        durable_settle_result(&mut store, &ids, &ok_result(), true, 1_100);
        let record = store
            .client_request_record(KIND, &ids.key_digest)
            .unwrap()
            .unwrap();
        assert_eq!(record.expires_at, Some(1_100 + REQUEST_LEDGER_TTL_MS));

        // After the TTL the kind cleanup deletes the row; the key reserves
        // again because a read has no side effect to protect.
        let past = 1_100 + REQUEST_LEDGER_TTL_MS + 1;
        let outcome = durable_begin_or_replay(
            &mut store,
            &ids,
            KEY,
            SCOPE_A,
            crate::progressive_skills::REQUEST_STATUS_METHOD,
            &params,
            true,
            past,
        );
        expect_execute(outcome);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given two stores on one state.db racing one key | When
    /// both reserve | Then only one wins; the other sees the in-flight row.
    #[test]
    fn user_submit_given_two_stores_one_key_when_reserved_then_single_winner() {
        let dir = temp_state_dir();
        let params = json!({"continuity_id": "wm:a"});
        let mut first = open_store(&dir);
        let mut second = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_cc_1", false);
        expect_execute(begin(&mut first, &ids, SCOPE_A, &params, false, 1_000));
        // The second process sees a pending row from a foreign boot.
        let outcome = durable_begin_or_replay(
            &mut second,
            &ids,
            KEY,
            SCOPE_A,
            METHOD,
            &params,
            false,
            1_010,
        );
        let body = respond_body(outcome);
        assert_eq!(body["code"], "delivery_unknown", "{body}");
        assert!(
            first
                .client_request_record(KIND, &ids.key_digest)
                .unwrap()
                .is_some()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given method names | When classified for durable
    /// gating | Then only the allowlisted side-effect-free methods are reads;
    /// everything else is a write.
    #[test]
    fn user_method_classification_given_methods_when_classified_then_fail_closed() {
        for read in [
            "server.status",
            " Server.Status ",
            "herdr_mcp.request.status",
            "herdr_mcp.skill.list",
            "herdr_mcp.skill.describe",
            "herdr_mcp.skill.load",
            "herdr_mcp.github.status",
            "herdr_mcp.cleanup.preview",
            "herdr_mcp.browser_endpoint.list",
            "herdr_mcp.browser_endpoint.inspect",
            "herdr_mcp.browser_resource.list",
            "herdr_mcp.browser_resource.inspect",
            "work_memory.search",
            "work_memory.resume",
            "herdr_mcp.codex_session.list",
            "herdr_mcp.codex_session.read",
            "herdr_mcp.agent.task.status",
            "herdr_mcp.agent.task.inbox",
            "herdr_mcp.agent.task.gate.status",
            "herdr_mcp.text.read",
            "herdr_mcp.bilibili.video.transcript",
            "herdr_mcp.doubao.image.status",
            "herdr_mcp.x.search.posts",
            "herdr_mcp.x.thread.read",
        ] {
            assert!(
                is_read_only_client_request_method(read),
                "{read} should be read-only"
            );
        }
        for write in [
            "pane.split",
            "work.pickup",
            "workstation.preflight",
            "work_memory.bind",
            "work_memory.append_turn",
            "work_memory.checkpoint.put",
            "herdr_mcp.agent.task.dispatch",
            "herdr_mcp.agent.task.report",
            "herdr_mcp.agent.task.ack",
            "herdr_mcp.agent.task.gate.create",
            "herdr_mcp.agent.task.gate.resolve",
            "herdr_mcp.page_assist",
            "herdr_mcp.browser_page.action",
            "herdr_mcp.browser_page.fast_path",
            "herdr_mcp.browser_page.lifecycle",
            "herdr_mcp.handoff.prepare",
            "herdr_mcp.source.resolve",
            "herdr_mcp.exec.wait",
            "herdr_mcp.codex_session.resume",
            "herdr_mcp.text.write",
            "herdr_mcp.browser_resource.resolve",
            "herdr_mcp.doubao.image.generate",
            "herdr_mcp.planning.advise",
            "herdr_mcp.validation.advise",
            "herdr_mcp.agent.closeout.advise",
            "herdr_mcp.agent.attention.advise",
            "continuity.resume",
            "continuity.resolve",
            "continuity.search",
            "artifact.write",
            "herdr_mcp.method.not.invented.yet",
        ] {
            assert!(
                !is_read_only_client_request_method(write),
                "{write} must classify as write"
            );
        }
    }

    /// user caller | Given a settled request whose first response carried
    /// receipt identifiers | When the same key retries | Then the replay
    /// returns the same task_id/receipt_id while body payload stays out of
    /// the projection.
    #[test]
    fn user_prompt_retry_then_replay_returns_same_receipt_identifiers() {
        let dir = temp_state_dir();
        let params = json!({"pane": "w1:p1", "prompt": "do it"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_prompt_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        durable_settle_result(
            &mut store,
            &ids,
            &json!({
                "ok": true,
                "task_id": "task_1",
                "receipt_id": "rcpt_1",
                "prompt_echo": "body content that must not persist",
            }),
            false,
            1_100,
        );

        let retry = ids_for(SCOPE_A, &params, "req_prompt_2", false);
        let replay = replay_body(begin(&mut store, &retry, SCOPE_A, &params, false, 2_000));
        assert_eq!(replay["deduplicated"], true, "{replay}");
        assert_eq!(replay["task_id"], "task_1", "{replay}");
        assert_eq!(replay["receipt_id"], "rcpt_1", "{replay}");
        assert!(
            !replay.to_string().contains("prompt_echo"),
            "receipt whitelist must not retain body fields: {replay}"
        );
        // request.status surfaces the same receipt for inspection.
        let status = durable_client_request_status(&store, KEY, SCOPE_A, None).unwrap();
        assert_eq!(status["receipt"]["task_id"], "task_1", "{status}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a commit failure whose rollback itself errored |
    /// When the result settles | Then a delivery_unknown tombstone persists
    /// and a same-key retry is refused with inspect_before_retry instead of
    /// re-executing a possibly-applied write.
    #[test]
    fn user_write_retry_given_partial_rollback_then_inspect_before_retry() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt"});
        let mut store = open_store(&dir);
        let ids = ids_for(SCOPE_A, &params, "req_patch_1", false);
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        assert_eq!(
            durable_settle_result(
                &mut store,
                &ids,
                &json!({
                    "ok": false,
                    "code": "PATCH_COMMIT_FAILED",
                    "message": "stage failed; rollback errors: remove a.txt: busy",
                }),
                false,
                1_100,
            ),
            DurableSettle::RecordedUnknown
        );

        let retry = ids_for(SCOPE_A, &params, "req_patch_2", false);
        let body = respond_body(begin(&mut store, &retry, SCOPE_A, &params, false, 2_000));
        assert_eq!(body["safe_retry_mode"], "inspect_before_retry", "{body}");
        assert_eq!(body["delivery_state"], "delivery_unknown", "{body}");

        // A clean rollback (no rollback errors) stays rejected: under a
        // different key the reservation releases and a retry reserves fresh.
        let clean_params = json!({"path": "b.txt"});
        let clean = client_request_ids(
            KIND,
            SCOPE_A,
            METHOD,
            &clean_params,
            "cr-key-2",
            "req_patch_3",
            false,
        );
        expect_execute(durable_begin_or_replay(
            &mut store,
            &clean,
            "cr-key-2",
            SCOPE_A,
            METHOD,
            &clean_params,
            false,
            3_000,
        ));
        durable_settle_result(
            &mut store,
            &clean,
            &json!({"ok": false, "code": "PATCH_COMMIT_FAILED", "message": "stage failed"}),
            false,
            3_100,
        );
        let retry = client_request_ids(
            KIND,
            SCOPE_A,
            METHOD,
            &clean_params,
            "cr-key-2",
            "req_patch_4",
            false,
        );
        expect_execute(durable_begin_or_replay(
            &mut store,
            &retry,
            "cr-key-2",
            SCOPE_A,
            METHOD,
            &clean_params,
            false,
            4_000,
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given the same key stored under two kinds and two
    /// scopes | When request.status runs per scope | Then each scope reads
    /// its own record instead of whichever kind scans first.
    #[test]
    fn user_status_given_same_key_across_kinds_then_scope_selects_record() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt"});
        let mut store = open_store(&dir);
        for (kind, scope, request_id) in [
            (HERDR_CALL_CLIENT_REQUEST_KIND, SCOPE_A, "req_call_1"),
            (HERDR_FS_WRITE_CLIENT_REQUEST_KIND, SCOPE_B, "req_write_1"),
        ] {
            let ids = client_request_ids(kind, scope, METHOD, &params, KEY, request_id, false);
            expect_execute(begin(&mut store, &ids, scope, &params, false, 1_000));
            durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);
        }
        let status_a = durable_client_request_status(&store, KEY, SCOPE_A, None).unwrap();
        assert_eq!(status_a["request_id"], "req_call_1", "{status_a}");
        let status_b = durable_client_request_status(&store, KEY, SCOPE_B, None).unwrap();
        assert_eq!(status_b["request_id"], "req_write_1", "{status_b}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given the same key settled under two write-tool kinds in
    /// one scope | When request.status runs without a tool hint | Then it
    /// reports the ambiguity instead of answering for whichever row is newer,
    /// and a `tool` hint selects the intended record.
    #[test]
    fn user_status_given_key_shared_by_two_tools_then_ambiguous_until_tool_hint() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt"});
        let mut store = open_store(&dir);
        for (kind, request_id) in [
            (HERDR_FS_WRITE_CLIENT_REQUEST_KIND, "req_w_1"),
            (HERDR_FS_PATCH_CLIENT_REQUEST_KIND, "req_p_1"),
        ] {
            let ids = client_request_ids(kind, SCOPE_A, METHOD, &params, KEY, request_id, false);
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
            durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);
        }
        let status = durable_client_request_status(&store, KEY, SCOPE_A, None).unwrap();
        assert_eq!(status["ambiguous"], true, "{status}");
        assert_eq!(status["matches"].as_array().unwrap().len(), 2, "{status}");
        let write =
            durable_client_request_status(&store, KEY, SCOPE_A, Some("herdr_fs_write")).unwrap();
        assert_eq!(write["request_id"], "req_w_1", "{write}");
        let patch =
            durable_client_request_status(&store, KEY, SCOPE_A, Some("herdr_fs_patch")).unwrap();
        assert_eq!(patch["request_id"], "req_p_1", "{patch}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a first-class write tool settled under its own
    /// kind | When request.status is queried by the first call's request_id
    /// after a restart | Then the durable row is found — the request_id scan
    /// covers every client-request kind, not only herdr_call/herdr_exec.
    #[test]
    fn user_status_by_request_id_given_write_tool_then_found_after_restart() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt", "content": "x"});
        {
            let mut store = open_store(&dir);
            let ids = client_request_ids(
                HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
                SCOPE_A,
                "herdr_fs_write",
                &params,
                KEY,
                "req_write_restart",
                false,
            );
            expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
            durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);
        }
        let store = open_store(&dir);
        let records = store
            .client_request_records_for_request_id("req_write_restart")
            .unwrap();
        assert_eq!(records.len(), 1, "request_id row must survive restart");
        let status =
            durable_client_request_status(&store, "req_write_restart", SCOPE_A, None).unwrap();
        assert_eq!(status["found"], true, "{status}");
        assert_eq!(status["state"], "complete", "{status}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a real SQLite write failure at settle time | When
    /// an executed write tool's terminal record cannot be persisted | Then
    /// the settlement reports `PersistFailed` and the reservation row stays
    /// pending — never a `Completed` verdict the caller would trust.
    #[test]
    fn user_settle_given_real_write_failure_then_persist_failed_keeps_pending() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt", "content": "x"});
        let mut store = open_store(&dir);
        let ids = client_request_ids(
            HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
            SCOPE_A,
            "herdr_fs_write",
            &params,
            KEY,
            "req_fail_1",
            false,
        );
        expect_execute(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        // Break every subsequent write through a genuine second connection:
        // renaming the table is a real SQLite DDL failure surface, not a
        // mocked store.
        let breaker = rusqlite::Connection::open(dir.join("state.db")).unwrap();
        breaker
            .execute_batch("ALTER TABLE operations RENAME TO operations_broken")
            .unwrap();
        drop(breaker);
        let settled = durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);
        assert_eq!(settled, DurableSettle::PersistFailed);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given one request_id carried by records in different
    /// scopes and kinds | When request.status resolves the id | Then every
    /// scope sees its own record, a same-scope cross-kind pair reports
    /// ambiguity, and a tool hint disambiguates — a foreign row never
    /// shadows the caller's.
    #[test]
    fn user_status_given_request_id_shared_across_scopes_then_each_scope_finds_own() {
        let dir = temp_state_dir();
        let params = json!({});
        let mut store = open_store(&dir);
        // Scope A holds the id under two kinds (genuinely ambiguous); scope B
        // under one. Scope B must use a different raw key — the key digest is
        // scope-blind, so reusing scope A's key would hit idempotency_conflict
        // at reservation time. Before collecting all candidates the first
        // scanned row could shadow the caller's own.
        for (kind, scope, key, req) in [
            (
                HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
                SCOPE_A,
                KEY,
                "req_shared",
            ),
            (HERDR_PROMPT_CLIENT_REQUEST_KIND, SCOPE_A, KEY, "req_shared"),
            (
                HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
                SCOPE_B,
                "key-b",
                "req_shared",
            ),
        ] {
            let ids = client_request_ids(kind, scope, METHOD, &params, key, req, false);
            expect_execute(durable_begin_or_replay(
                &mut store, &ids, key, scope, METHOD, &params, false, 1_000,
            ));
            durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);
        }
        // Scope B: exactly one candidate under its scope — found, not
        // shadowed by scope A's earlier rows.
        let status_b = durable_client_request_status(&store, "req_shared", SCOPE_B, None).unwrap();
        assert_eq!(status_b["found"], true, "{status_b}");
        assert_ne!(status_b["ambiguous"], true, "{status_b}");
        // Scope A: both kinds match — reported ambiguous, then a tool hint
        // resolves to the fs_write record.
        let status_a = durable_client_request_status(&store, "req_shared", SCOPE_A, None).unwrap();
        assert_eq!(status_a["found"], true, "{status_a}");
        assert_eq!(status_a["ambiguous"], true, "{status_a}");
        let hinted =
            durable_client_request_status(&store, "req_shared", SCOPE_A, Some("herdr_fs_write"))
                .unwrap();
        assert_eq!(hinted["found"], true, "{hinted}");
        assert_ne!(hinted["ambiguous"], true, "{hinted}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given first-class write-tool rows filling the ledger |
    /// When the shared capacity limit is reached | Then the next
    /// reservation is refused — the new kinds count toward the same
    /// 1024-row budget and are not exempt from pruning or compaction.
    #[test]
    fn user_reservation_given_new_kinds_fill_capacity_then_refused() {
        let dir = temp_state_dir();
        let params = json!({});
        let mut store = open_store(&dir);
        for i in 0..CLIENT_REQUEST_LEDGER_MAX_ROWS {
            let kind = CLIENT_REQUEST_KINDS[(i as usize) % CLIENT_REQUEST_KINDS.len()];
            let key = format!("cap-key-{i}");
            let ids = client_request_ids(kind, SCOPE_A, METHOD, &params, &key, "req_cap", false);
            match durable_begin_or_replay(
                &mut store, &ids, &key, SCOPE_A, METHOD, &params, false, 1_000,
            ) {
                ClientRequestOutcome::Execute { .. } => {}
                ClientRequestOutcome::Replay(v) => panic!("row {i} replayed: {v}"),
                ClientRequestOutcome::Respond(v) => panic!("row {i} responded: {v}"),
            }
        }
        let ids = ids_for(SCOPE_A, &params, "req_cap_over", false);
        let body = respond_body(begin(&mut store, &ids, SCOPE_A, &params, false, 1_000));
        assert_eq!(body["code"], "client_request_ledger_full", "{body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// user caller | Given a settled first-class write-tool summary past its
    /// deadline | When the next reservation triggers cleanup | Then the
    /// new-kind row compacts to the same hash-only tombstone as herdr_call.
    #[test]
    fn user_retry_given_expired_summary_on_write_tool_kind_then_tombstoned() {
        let dir = temp_state_dir();
        let params = json!({"path": "a.txt"});
        let mut store = open_store(&dir);
        let ids = client_request_ids(
            HERDR_FS_WRITE_CLIENT_REQUEST_KIND,
            SCOPE_A,
            "herdr_fs_write",
            &params,
            KEY,
            "req_t_fw",
            false,
        );
        expect_execute(durable_begin_or_replay(
            &mut store,
            &ids,
            KEY,
            SCOPE_A,
            "herdr_fs_write",
            &params,
            false,
            1_000,
        ));
        durable_settle_result(&mut store, &ids, &ok_result(), false, 1_100);

        // A later reservation (any kind) must run the same compaction pass
        // over the fs_write row.
        let past = 1_100 + CLIENT_REQUEST_SUMMARY_TTL_MS + 1;
        let other = ids_for(SCOPE_A, &params, "req_t_other", false);
        let _ = begin(&mut store, &other, SCOPE_A, &params, false, past);
        let record = store
            .client_request_record(HERDR_FS_WRITE_CLIENT_REQUEST_KIND, &ids.key_digest)
            .unwrap()
            .unwrap();
        assert_eq!(record.state.as_deref(), Some("complete"));
        assert_eq!(record.expires_at, None, "tombstone must stay unprunable");
        let result = record.result();
        assert_eq!(result["expired"], true, "{result}");
        assert_eq!(result["outcome"], "expired", "{result}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
