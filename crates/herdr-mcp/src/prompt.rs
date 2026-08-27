use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::state_store::{OperationReservation, StateStore};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::thread;

const RECORD_TTL_MS: u64 = 10 * 60_000;
const RECORD_LIMIT: usize = 512;
const PROMPT_OPERATION_KIND: &str = "herdr_prompt";
const MAX_REPLAY_JSON_BYTES: usize = 128 * 1024;
const STATE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WAIT_MS: u64 = 25_000;
const MAX_WAIT_MS: u64 = 60_000;
const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Eq, PartialEq)]
struct AgentState {
    pane_id: Option<String>,
    agent_status: Option<String>,
    state_change_seq: Option<u64>,
}

#[derive(Debug, Clone)]
enum RecordState {
    Pending,
    Complete(Value),
}

#[derive(Debug, Clone)]
struct PromptRecord {
    at_ms: u64,
    fingerprint: String,
    state: RecordState,
}

#[derive(Debug, Default)]
struct PromptRegistryInner {
    records: Mutex<HashMap<String, PromptRecord>>,
    store: Option<Arc<Mutex<StateStore>>>,
}

#[derive(Debug, Clone, Default)]
pub struct PromptRegistry {
    inner: Arc<PromptRegistryInner>,
}

#[derive(Debug)]
enum BeginPrompt {
    Reserved(Option<String>),
    Replay(Value),
}

impl PromptRegistry {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_store(store: Arc<Mutex<StateStore>>) -> Self {
        Self {
            inner: Arc::new(PromptRegistryInner {
                records: Mutex::new(HashMap::new()),
                store: Some(store),
            }),
        }
    }

    fn begin(&self, key: &str, fingerprint: &str) -> Result<BeginPrompt, Value> {
        if let Some(store) = &self.inner.store {
            return begin_persisted(store, key, fingerprint);
        }
        self.begin_memory(key, fingerprint)
    }

    fn begin_memory(&self, key: &str, fingerprint: &str) -> Result<BeginPrompt, Value> {
        let now = now_ms();
        let mut records = self
            .inner
            .records
            .lock()
            .map_err(|_| json!({"ok": false, "code": "idempotency_registry_unavailable"}))?;
        records.retain(|_, record| now.saturating_sub(record.at_ms) < RECORD_TTL_MS);
        if let Some(record) = records.get(key) {
            if record.fingerprint != fingerprint {
                return Err(json!({
                    "ok": false,
                    "code": "idempotency_key_conflict",
                    "message": "idempotency_key is already bound to a different prompt request",
                    "retryable": false,
                }));
            }
            return match &record.state {
                RecordState::Pending => Err(json!({
                    "ok": false,
                    "code": "idempotency_in_flight",
                    "message": "a prompt with this idempotency_key is already in flight",
                    "submitted": "unknown",
                    "retryable": false,
                    "hint": "wait for the first call result or inspect agent state; do not submit the same prompt again",
                })),
                RecordState::Complete(result) => {
                    let mut replay = result.clone();
                    if let Some(object) = replay.as_object_mut() {
                        object.insert("idempotent_replay".to_owned(), json!(true));
                    }
                    Ok(BeginPrompt::Replay(replay))
                }
            };
        }
        records.insert(
            key.to_owned(),
            PromptRecord {
                at_ms: now,
                fingerprint: fingerprint.to_owned(),
                state: RecordState::Pending,
            },
        );
        if records.len() > RECORD_LIMIT {
            let mut oldest = records
                .iter()
                .filter(|(candidate, _)| candidate.as_str() != key)
                .map(|(candidate, record)| (candidate.clone(), record.at_ms))
                .collect::<Vec<_>>();
            oldest.sort_by_key(|(_, at)| *at);
            for (candidate, _) in oldest.into_iter().take(records.len() - RECORD_LIMIT) {
                records.remove(&candidate);
            }
        }
        Ok(BeginPrompt::Reserved(None))
    }

    fn complete(&self, key: &str, fingerprint: &str, result: &Value) -> Result<(), Value> {
        if let Some(store) = &self.inner.store {
            return complete_persisted(store, key, fingerprint, result);
        }
        if let Ok(mut records) = self.inner.records.lock() {
            records.insert(
                key.to_owned(),
                PromptRecord {
                    at_ms: now_ms(),
                    fingerprint: fingerprint.to_owned(),
                    state: RecordState::Complete(result.clone()),
                },
            );
        }
        Ok(())
    }
}

fn begin_persisted(
    store: &Arc<Mutex<StateStore>>,
    key: &str,
    fingerprint: &str,
) -> Result<BeginPrompt, Value> {
    let now = now_ms();
    let expires_at = now.saturating_add(RECORD_TTL_MS);
    let key_hash = stable_digest(&[b"herdr_prompt/idempotency", key.as_bytes()]);
    let op_id = format!("op:prompt:{}", &key_hash[..32]);
    let mut store = store.lock().map_err(|_| {
        json!({
            "ok": false,
            "code": "idempotency_store_unavailable",
            "message": "durable idempotency store lock is unavailable; prompt was not submitted",
            "retryable": false,
        })
    })?;
    let reservation = store
        .reserve_operation(
            PROMPT_OPERATION_KIND,
            &key_hash,
            fingerprint,
            &op_id,
            i64::try_from(now).unwrap_or(i64::MAX),
            i64::try_from(expires_at).unwrap_or(i64::MAX),
        )
        .map_err(|error| {
            json!({
                "ok": false,
                "code": "idempotency_store_unavailable",
                "message": format!("durable idempotency reservation failed: {error}"),
                "retryable": false,
                "hint": "prompt was not submitted; repair the local state store before retrying",
            })
        })?;

    match reservation {
        OperationReservation::Reserved => Ok(BeginPrompt::Reserved(Some(op_id))),
        OperationReservation::Existing(record) => {
            if record.request_hash != fingerprint {
                return Err(json!({
                    "ok": false,
                    "code": "idempotency_key_conflict",
                    "message": "idempotency_key is already bound to a different prompt request",
                    "retryable": false,
                    "op_id": record.op_id,
                }));
            }
            match record.state.as_deref() {
                Some("pending") => Err(json!({
                    "ok": false,
                    "code": "idempotency_in_flight",
                    "message": "a prompt with this idempotency_key is already reserved or may have been submitted",
                    "submitted": "unknown",
                    "retryable": false,
                    "op_id": record.op_id,
                    "hint": "inspect agent/live state before any retry; a runtime restart does not clear this reservation",
                })),
                Some("complete") => {
                    let result_json = record.result_json.ok_or_else(|| {
                        json!({
                            "ok": false,
                            "code": "idempotency_record_corrupt",
                            "message": "completed idempotency record has no replay payload",
                            "retryable": false,
                            "op_id": record.op_id,
                        })
                    })?;
                    let mut replay: Value = serde_json::from_str(&result_json).map_err(|error| {
                        json!({
                            "ok": false,
                            "code": "idempotency_record_corrupt",
                            "message": format!("completed idempotency replay payload is invalid: {error}"),
                            "retryable": false,
                            "op_id": record.op_id,
                        })
                    })?;
                    let object = replay.as_object_mut().ok_or_else(|| {
                        json!({
                            "ok": false,
                            "code": "idempotency_record_corrupt",
                            "message": "completed idempotency replay payload is not an object",
                            "retryable": false,
                            "op_id": record.op_id,
                        })
                    })?;
                    object.insert("idempotent_replay".to_owned(), json!(true));
                    object.insert("idempotency_persisted".to_owned(), json!(true));
                    object.insert("op_id".to_owned(), json!(record.op_id));
                    Ok(BeginPrompt::Replay(replay))
                }
                other => Err(json!({
                    "ok": false,
                    "code": "idempotency_record_corrupt",
                    "message": format!("idempotency record has unsupported state {other:?}"),
                    "retryable": false,
                    "op_id": record.op_id,
                })),
            }
        }
    }
}

fn complete_persisted(
    store: &Arc<Mutex<StateStore>>,
    key: &str,
    fingerprint: &str,
    result: &Value,
) -> Result<(), Value> {
    let result_json = serde_json::to_string(result).map_err(|error| {
        json!({
            "ok": false,
            "code": "idempotency_completion_persist_failed",
            "message": format!("cannot encode prompt replay payload: {error}"),
            "retryable": false,
        })
    })?;
    if result_json.len() > MAX_REPLAY_JSON_BYTES {
        return Err(json!({
            "ok": false,
            "code": "idempotency_completion_persist_failed",
            "message": format!("prompt replay payload exceeds {} bytes", MAX_REPLAY_JSON_BYTES),
            "retryable": false,
            "hint": "the prompt may already have been submitted; inspect live state and do not blindly retry",
        }));
    }
    let now = now_ms();
    let expires_at = now.saturating_add(RECORD_TTL_MS);
    let key_hash = stable_digest(&[b"herdr_prompt/idempotency", key.as_bytes()]);
    let mut store = store.lock().map_err(|_| {
        json!({
            "ok": false,
            "code": "idempotency_completion_persist_failed",
            "message": "durable idempotency store lock is unavailable after prompt execution",
            "retryable": false,
            "hint": "the prompt may already have been submitted; inspect live state and do not blindly retry",
        })
    })?;
    store
        .complete_operation(
            PROMPT_OPERATION_KIND,
            &key_hash,
            fingerprint,
            &result_json,
            i64::try_from(now).unwrap_or(i64::MAX),
            i64::try_from(expires_at).unwrap_or(i64::MAX),
        )
        .map_err(|error| {
            json!({
                "ok": false,
                "code": "idempotency_completion_persist_failed",
                "message": format!("durable idempotency completion failed: {error}"),
                "retryable": false,
                "hint": "the prompt may already have been submitted; inspect live state and do not blindly retry",
            })
        })
}

fn annotate_idempotency_persistence(mut result: Value, persistence_error: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("idempotency_persistence".to_owned(), persistence_error);
        object.insert("idempotency_persisted".to_owned(), json!(false));
        object.insert(
            "idempotency_hint".to_owned(),
            json!("prompt outcome may already be committed while durable replay metadata is incomplete; inspect live state before retrying"),
        );
    }
    result
}

pub fn run(client: &HerdrClient, registry: &PromptRegistry, args: &Value) -> Value {
    if let Err(error) = mutation::check_global("herdr_prompt") {
        return error;
    }
    let target = match required_str(args, "target") {
        Ok("") => return invalid("target must not be empty"),
        Ok(value) => value,
        Err(error) => return error,
    };
    let text = match required_str(args, "text") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let idempotency_key = match optional_str(args, "idempotency_key") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let wait = match parse_wait(args.get("wait")) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let fingerprint = request_fingerprint(target, text, wait.as_ref());
    let mut operation_id = None;

    if let Some(key) = idempotency_key {
        match registry.begin(key, &fingerprint) {
            Ok(BeginPrompt::Reserved(op_id)) => operation_id = op_id,
            Ok(BeginPrompt::Replay(result)) => return result,
            Err(error) => return error,
        }
    }

    let before = agent_state_of(client, target);
    let params = json!({
        "target": target,
        "text": text,
        "wait": wait.clone().unwrap_or(Value::Null),
    });
    let wait_timeout_ms = wait
        .as_ref()
        .and_then(|value| value.get("timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_WAIT_MS);
    let call_timeout_ms = if wait.is_some() {
        wait_timeout_ms.saturating_add(5_000).min(MAX_WAIT_MS)
    } else {
        DEFAULT_CALL_TIMEOUT_MS
    };

    let mut result = match client.call_with_timeout(
        "agent.prompt",
        params,
        Duration::from_millis(call_timeout_ms),
    ) {
        Ok(response) => prompt_success(
            client,
            target,
            before.as_ref(),
            wait.is_some(),
            response,
            idempotency_key.is_none(),
        ),
        Err(error) => prompt_failure(client, target, before.as_ref(), wait.is_some(), error),
    };

    if let Some(op_id) = operation_id
        && let Some(object) = result.as_object_mut()
    {
        object.insert("op_id".to_owned(), json!(op_id));
    }

    if let Some(key) = idempotency_key {
        if let Err(persist_error) = registry.complete(key, &fingerprint, &result) {
            return annotate_idempotency_persistence(result, persist_error);
        }
        if registry.inner.store.is_some()
            && let Some(object) = result.as_object_mut()
        {
            object.insert("idempotency_persisted".to_owned(), json!(true));
        }
    }
    result
}

fn prompt_success(
    client: &HerdrClient,
    target: &str,
    before: Option<&AgentState>,
    waited: bool,
    response: Value,
    needs_idempotency_hint: bool,
) -> Value {
    let prompt = response.get("prompt").cloned().unwrap_or(response);
    let status = prompt
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("submitted")
        .to_owned();
    // The native agent.prompt success response normally includes current agent
    // state. Reuse that delivery evidence instead of forcing an additional
    // agent.get round trip. Legacy responses that omit agent state keep one
    // bounded fallback probe so the existing before/after evidence remains
    // available, but the historical 250ms sleep + second probe is removed.
    let mut after = agent_state_from_value(&prompt);
    if after.is_none() {
        after = agent_state_of(client, target);
    }
    let observation = build_state_observation(before, after.as_ref(), waited);
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(true));
    result.insert("target".to_owned(), json!(target));
    result.insert(
        "resolved_pane".to_owned(),
        json!(
            after
                .as_ref()
                .and_then(|state| state.pane_id.as_deref())
                .or_else(|| before.and_then(|state| state.pane_id.as_deref()))
        ),
    );
    result.insert("status".to_owned(), json!(status));
    result.insert("submitted".to_owned(), json!(status != "agent_blocked"));
    result.insert("before".to_owned(), state_view(before));
    result.insert("after".to_owned(), state_view(after.as_ref()));
    insert_observation(&mut result, observation);
    if !waited {
        result.insert(
            "seq_note".to_owned(),
            json!("seq may lag; state_observation.changed=unknown does NOT prove non-delivery"),
        );
    }
    result.insert("prompt".to_owned(), prompt);
    if needs_idempotency_hint {
        result.insert(
            "idempotency_hint".to_owned(),
            json!("pass idempotency_key on mutating prompts to make retries safe"),
        );
    }
    Value::Object(result)
}

fn prompt_failure(
    client: &HerdrClient,
    target: &str,
    before: Option<&AgentState>,
    waited: bool,
    error: HerdrError,
) -> Value {
    let after = agent_state_of(client, target);
    let resolve_failure = matches!(
        error.code.as_str(),
        "agent_not_found" | "unknown_agent" | "unknown_pane"
    );
    if is_agent_status_wait_timeout(&error.message) {
        let submitted = inferred_submission(before, after.as_ref());
        let observation = build_state_observation(before, after.as_ref(), true);
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(false));
        result.insert("target".to_owned(), json!(target));
        result.insert("failure".to_owned(), json!("agent_status_wait_timeout"));
        result.insert(
            "failure_phase".to_owned(),
            json!("post_submission_status_wait"),
        );
        result.insert("submitted".to_owned(), submitted.clone());
        result.insert(
            "delivery_uncertain".to_owned(),
            json!(submitted == json!("unknown")),
        );
        result.insert(
            "resolved_pane".to_owned(),
            json!(resolved_pane(before, after.as_ref())),
        );
        result.insert("before".to_owned(), state_view(before));
        result.insert("after".to_owned(), state_view(after.as_ref()));
        insert_observation(&mut result, observation);
        result.insert("code".to_owned(), json!(error.code));
        result.insert("message".to_owned(), json!(error.message));
        result.insert("retryable".to_owned(), json!(false));
        result.insert(
            "hint".to_owned(),
            json!("status wait timed out after accept — verify with herdr_inspect / herdr_since before re-sending"),
        );
        result.insert(
            "wait".to_owned(),
            json!({"completed": false, "reason": "agent_status_timeout"}),
        );
        return Value::Object(result);
    }

    if is_control_plane_taskgroup(&error.message) {
        let submitted = inferred_submission(before, after.as_ref());
        let observation = build_state_observation(before, after.as_ref(), waited);
        let root_message = unwrap_control_plane_message(&error.message);
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(false));
        result.insert("target".to_owned(), json!(target));
        result.insert("failure".to_owned(), json!("herdr_internal"));
        result.insert("failure_phase".to_owned(), json!("control_plane_taskgroup"));
        result.insert("code".to_owned(), json!("control_plane_taskgroup"));
        result.insert("submitted".to_owned(), submitted.clone());
        result.insert(
            "delivery_uncertain".to_owned(),
            json!(submitted != json!(true)),
        );
        result.insert(
            "resolved_pane".to_owned(),
            json!(resolved_pane(before, after.as_ref())),
        );
        result.insert("before".to_owned(), state_view(before));
        result.insert("after".to_owned(), state_view(after.as_ref()));
        insert_observation(&mut result, observation);
        result.insert("message".to_owned(), json!(root_message));
        result.insert(
            "error".to_owned(),
            json!({
                "type": if error.message.to_ascii_lowercase().contains("exceptiongroup") { "ExceptionGroup" } else { "TaskGroup" },
                "message": root_message,
                "raw": truncate_chars(&error.message, 2_000),
            }),
        );
        result.insert("retryable".to_owned(), json!(false));
        result.insert(
            "hint".to_owned(),
            json!("herdr daemon control-plane TaskGroup blip on agent.prompt — check herdr_since / herdr_inspect before any re-prompt; do not treat this as agent dead or as a project failure"),
        );
        return Value::Object(result);
    }

    let observation = build_state_observation(before, after.as_ref(), waited);
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("target".to_owned(), json!(target));
    result.insert(
        "failure_phase".to_owned(),
        json!(if resolve_failure {
            "resolve"
        } else {
            "submit_or_response_lost"
        }),
    );
    result.insert(
        "resolved_pane".to_owned(),
        json!(resolved_pane(before, after.as_ref())),
    );
    result.insert("before".to_owned(), state_view(before));
    result.insert("after".to_owned(), state_view(after.as_ref()));
    insert_observation(&mut result, observation);
    result.insert("code".to_owned(), json!(error.code));
    result.insert("message".to_owned(), json!(error.message));
    let retryable = definitely_not_delivered(&error);
    result.insert("retryable".to_owned(), json!(retryable));
    if !resolve_failure && !retryable {
        result.insert("delivery_uncertain".to_owned(), json!(true));
        result.insert(
            "hint".to_owned(),
            json!("agent.prompt may have been delivered before the response was lost — verify with herdr_inspect / herdr_since and do not blind-retry"),
        );
    }
    Value::Object(result)
}

fn agent_state_of(client: &HerdrClient, target: &str) -> Option<AgentState> {
    let result = client
        .call_with_timeout("agent.get", json!({"target": target}), STATE_PROBE_TIMEOUT)
        .ok()?;
    agent_state_from_value(&result)
}

fn agent_state_from_value(value: &Value) -> Option<AgentState> {
    let agent = value.get("agent").unwrap_or(value);
    let state = AgentState {
        pane_id: agent
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_status: agent
            .get("agent_status")
            .and_then(Value::as_str)
            .or_else(|| agent.get("status").and_then(Value::as_str))
            .map(str::to_owned),
        state_change_seq: agent.get("state_change_seq").and_then(Value::as_u64),
    };
    (state.pane_id.is_some() || state.agent_status.is_some() || state.state_change_seq.is_some())
        .then_some(state)
}

#[derive(Debug, Clone)]
struct Observation {
    changed: Value,
    fresh: bool,
    state_changed: bool,
}

fn build_state_observation(
    before: Option<&AgentState>,
    after: Option<&AgentState>,
    waited: bool,
) -> Observation {
    let state_changed = before.zip(after).is_some_and(|(before, after)| {
        before.state_change_seq != after.state_change_seq
            || before.agent_status != after.agent_status
    });
    let changed = if after.is_none() {
        json!("unknown")
    } else if state_changed {
        json!(true)
    } else if waited {
        json!(false)
    } else {
        json!("unknown")
    };
    Observation {
        changed,
        fresh: after.is_some() && (waited || state_changed),
        state_changed,
    }
}

fn insert_observation(result: &mut Map<String, Value>, observation: Observation) {
    result.insert(
        "state_observation".to_owned(),
        json!({"changed": observation.changed, "fresh": observation.fresh}),
    );
    result.insert("state_changed".to_owned(), json!(observation.state_changed));
}

fn inferred_submission(before: Option<&AgentState>, after: Option<&AgentState>) -> Value {
    let likely_working = after.and_then(|state| state.agent_status.as_deref()) == Some("working");
    let seq_moved = before
        .zip(after)
        .is_some_and(|(before, after)| before.state_change_seq != after.state_change_seq);
    if likely_working || seq_moved {
        json!(true)
    } else {
        json!("unknown")
    }
}

fn resolved_pane<'a>(
    before: Option<&'a AgentState>,
    after: Option<&'a AgentState>,
) -> Option<&'a str> {
    after
        .and_then(|state| state.pane_id.as_deref())
        .or_else(|| before.and_then(|state| state.pane_id.as_deref()))
}

fn state_view(state: Option<&AgentState>) -> Value {
    state.map_or(Value::Null, |state| {
        json!({
            "agent_status": state.agent_status,
            "state_change_seq": state.state_change_seq,
        })
    })
}

fn parse_wait(value: Option<&Value>) -> Result<Option<Value>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(invalid("wait must be an object"));
    };
    let mut normalized = Map::new();
    if let Some(until) = object.get("until") {
        let Some(statuses) = until.as_array() else {
            return Err(invalid("wait.until must be an array"));
        };
        let allowed = ["idle", "working", "blocked", "done", "unknown"];
        let mut values = Vec::with_capacity(statuses.len());
        for status in statuses {
            let Some(status) = status.as_str() else {
                return Err(invalid("wait.until entries must be strings"));
            };
            if !allowed.contains(&status) {
                return Err(invalid("wait.until contains an unsupported status"));
            }
            values.push(json!(status));
        }
        normalized.insert("until".to_owned(), Value::Array(values));
    }
    if let Some(timeout) = object.get("timeout_ms") {
        let Some(timeout) = timeout
            .as_u64()
            .filter(|timeout| (1..=MAX_WAIT_MS).contains(timeout))
        else {
            return Err(invalid("wait.timeout_ms must be an integer in 1..=60000"));
        };
        normalized.insert("timeout_ms".to_owned(), json!(timeout));
    }
    Ok(Some(Value::Object(normalized)))
}

fn request_fingerprint(target: &str, text: &str, wait: Option<&Value>) -> String {
    let wait_json = wait.map(Value::to_string).unwrap_or_default();
    stable_digest(&[
        b"herdr_prompt/request",
        target.as_bytes(),
        text.as_bytes(),
        wait_json.as_bytes(),
    ])
}

fn stable_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn definitely_not_delivered(error: &HerdrError) -> bool {
    matches!(error.code.as_str(), "socket_missing" | "connection_refused")
}

fn is_agent_status_wait_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timed out waiting for agent status")
        || lower.contains("waiting for agent status")
}

fn is_control_plane_taskgroup(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("exceptiongroup")
        || lower.contains("unhandled errors in a taskgroup")
        || (lower.contains("taskgroup")
            && (lower.contains("unhandled") || lower.contains("sub-exception")))
}

fn unwrap_control_plane_message(message: &str) -> String {
    let lines = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some(concrete) = lines.iter().find(|line| {
        let lower = line.to_ascii_lowercase();
        !lower.starts_with("exceptiongroup")
            && !lower.starts_with("unhandled errors in a taskgroup")
            && !lower.contains("sub-exception")
    }) {
        return (*concrete).to_owned();
    }
    "herdr control-plane TaskGroup blip (sub-exception not expanded by daemon)".to_owned()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(invalid(&format!("{key} must be a string"))),
    }
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_socket() -> PathBuf {
        env::temp_dir().join(format!(
            "herdr-mcp-prompt-test-{}-{}-{}.sock",
            std::process::id(),
            now_ms(),
            NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn idempotency_registry_prevents_parallel_and_conflicting_reuse() {
        let registry = PromptRegistry::new();
        assert!(matches!(
            registry.begin("k", "fp-1").unwrap(),
            BeginPrompt::Reserved(None)
        ));
        assert_eq!(
            registry.begin("k", "fp-1").unwrap_err()["code"],
            "idempotency_in_flight"
        );
        assert_eq!(
            registry.begin("k", "fp-2").unwrap_err()["code"],
            "idempotency_key_conflict"
        );
        registry
            .complete("k", "fp-1", &json!({"ok": true, "target": "pi"}))
            .unwrap();
        let BeginPrompt::Replay(replay) = registry.begin("k", "fp-1").unwrap() else {
            panic!("expected replay");
        };
        assert_eq!(replay["ok"], true);
        assert_eq!(replay["idempotent_replay"], true);
    }

    #[test]
    fn observation_distinguishes_unwaited_unknown_from_waited_false() {
        let before = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("idle".to_owned()),
            state_change_seq: Some(1),
        };
        let same = before.clone();
        assert_eq!(
            build_state_observation(Some(&before), Some(&same), false).changed,
            json!("unknown")
        );
        assert_eq!(
            build_state_observation(Some(&before), Some(&same), true).changed,
            json!(false)
        );
        let changed = AgentState {
            state_change_seq: Some(2),
            agent_status: Some("working".to_owned()),
            ..same
        };
        assert_eq!(
            build_state_observation(Some(&before), Some(&changed), false).changed,
            json!(true)
        );
    }

    #[test]
    fn wait_validation_matches_public_contract() {
        assert_eq!(
            parse_wait(Some(
                &json!({"until": ["working", "done"], "timeout_ms": 1000})
            ))
            .unwrap()
            .unwrap()["timeout_ms"],
            1000
        );
        assert_eq!(
            parse_wait(Some(&json!({"until": ["bogus"]}))).unwrap_err()["code"],
            "invalid_params"
        );
        assert_eq!(
            parse_wait(Some(&json!({"timeout_ms": 0}))).unwrap_err()["code"],
            "invalid_params"
        );
    }

    #[test]
    fn successful_prompt_uses_socket_agent_prompt_and_replays_without_resend() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for (index, expected) in ["agent.get", "agent.prompt"].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                let result = match (index, expected) {
                    (_, "agent.prompt") => json!({"prompt": {
                        "status": "submitted",
                        "agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 2}
                    }}),
                    (0, "agent.get") => json!({
                        "agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}
                    }),
                    _ => unreachable!(),
                };
                writeln!(stream, "{}", json!({"id": request["id"], "result": result})).unwrap();
            }
        });
        let client = HerdrClient::new(&socket);
        let registry = PromptRegistry::new();
        let args = json!({
            "target": "pi",
            "text": "do work",
            "idempotency_key": "prompt-test-1"
        });
        let first = run(&client, &registry, &args);
        assert_eq!(first["ok"], true);
        assert_eq!(first["submitted"], true);
        assert_eq!(first["resolved_pane"], "w1:p1");
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true);
        server.join().unwrap();
        fs::remove_file(socket).unwrap();
    }

    #[test]
    fn persisted_idempotency_replays_and_conflicts_after_registry_restart() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for (index, expected) in ["agent.get", "agent.prompt"].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                let result = match (index, expected) {
                    (_, "agent.prompt") => json!({"prompt": {
                        "status": "submitted",
                        "agent": {"pane_id": "w9:p1", "agent_status": "working", "state_change_seq": 11}
                    }}),
                    (0, "agent.get") => json!({
                        "agent": {"pane_id": "w9:p1", "agent_status": "idle", "state_change_seq": 10}
                    }),
                    _ => unreachable!(),
                };
                writeln!(stream, "{}", json!({"id": request["id"], "result": result})).unwrap();
            }
        });

        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-prompt-ledger-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let args = json!({
            "target": "pi",
            "text": "durable work",
            "idempotency_key": "prompt-persisted-1"
        });
        let client = HerdrClient::new(&socket);
        let first_registry =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&db_path).unwrap())));
        let first = run(&client, &first_registry, &args);
        assert_eq!(first["ok"], true);
        assert_eq!(first["idempotency_persisted"], true);
        assert!(first["op_id"].as_str().unwrap().starts_with("op:prompt:"));
        server.join().unwrap();
        fs::remove_file(&socket).unwrap();
        drop(first_registry);

        let second_registry =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&db_path).unwrap())));
        let unreachable = HerdrClient::new(&socket);
        let replay = run(&unreachable, &second_registry, &args);
        assert_eq!(replay["ok"], true);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["idempotency_persisted"], true);
        assert!(replay["op_id"].as_str().unwrap().starts_with("op:prompt:"));

        let conflict = run(
            &unreachable,
            &second_registry,
            &json!({
                "target": "pi",
                "text": "different durable work",
                "idempotency_key": "prompt-persisted-1"
            }),
        );
        assert_eq!(conflict["code"], "idempotency_key_conflict");
        assert_eq!(conflict["retryable"], false);

        drop(second_registry);
        fs::remove_file(&db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }
}
