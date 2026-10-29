use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::progressive_skills::agent_attention_advice;
use crate::state_cache::EventCache;
use crate::state_store::{
    OperationLedgerInput, OperationLedgerRecord, OperationReservation, StateStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECORD_TTL_MS: u64 = 10 * 60_000;
const RECORD_LIMIT: usize = 512;
const PROMPT_OPERATION_KIND: &str = "herdr_prompt";
const PROMPT_TASK_OPERATION_KIND: &str = "herdr_prompt_task";
const PROMPT_TASK_TTL_MS: u64 = 7 * 24 * 60 * 60_000;
const PROMPT_TASK_ACTIVITY_START_TIMEOUT_MS: u64 = 10_000;
const MAX_REPLAY_JSON_BYTES: usize = 128 * 1024;
const STATE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WAIT_MS: u64 = 25_000;
const MAX_WAIT_MS: u64 = 60_000;
/// Poll cadence for the bounded inbox long-poll (durable facts only).
const INBOX_WAIT_POLL: Duration = Duration::from_millis(250);
/// One batch ack may acknowledge at most this many tasks.
const MAX_TASK_ACK_BATCH: usize = 32;
const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;
/// Only task chains touched in the last 6h are live escalation evidence.
const ESCALATION_WINDOW_MS: u64 = 6 * 60 * 60_000;
/// 1 initial dispatch + 3 repair rounds exhausts one lane's mechanical budget.
const ESCALATION_REPAIR_BUDGET_TASKS: usize = 4;
/// Two consecutive failed/blocked terminals are repeated unsuccessful rounds.
const ESCALATION_UNSUCCESSFUL_STREAK: usize = 2;
/// A lane with no observed activity for 20min is worth a progress verify.
const ESCALATION_STALL_IDLE_MS: u64 = 20 * 60_000;
/// Bound the signal list so escalation hints cannot bloat the inbox payload.
const ESCALATION_MAX_SIGNALS: usize = 16;
/// Bound the per-signal task-id evidence (most recent first).
const ESCALATION_MAX_TASK_IDS: usize = 8;
static NEXT_DISPATCH_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Eq, PartialEq)]
struct AgentState {
    pane_id: Option<String>,
    agent_status: Option<String>,
    state_change_seq: Option<u64>,
}

struct RegisterTaskInput<'a> {
    task_id: &'a str,
    target: &'a str,
    fingerprint: &'a str,
    idempotency_key: Option<&'a str>,
    before: Option<&'a AgentState>,
    after: Option<&'a AgentState>,
    submitted: Option<bool>,
    start_cursor: u64,
    parent_target: Option<&'a str>,
    parent_session_ref: Option<&'a str>,
    stall_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PromptTaskLifecycle {
    AwaitingActivity,
    AwaitingPriorSettle,
    Running,
    Completed,
    Blocked,
    Failed,
    DeliveryUncertain,
}

impl PromptTaskLifecycle {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Failed | Self::DeliveryUncertain
        )
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::AwaitingActivity => "awaiting_activity",
            Self::AwaitingPriorSettle => "awaiting_prior_settle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::DeliveryUncertain => "delivery_uncertain",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PromptTaskRecord {
    schema_version: u32,
    task_id: String,
    dispatch_id: String,
    target: String,
    #[serde(default)]
    idempotency_key_digest: Option<String>,
    pane_id: Option<String>,
    workspace_id: Option<String>,
    baseline_status: Option<String>,
    baseline_seq: Option<u64>,
    start_cursor: u64,
    last_cursor: u64,
    lifecycle: PromptTaskLifecycle,
    activity_observed: bool,
    prior_settle_observed: bool,
    turn_id: Option<String>,
    exact_native_turn: bool,
    terminal_status: Option<String>,
    terminal_cursor: Option<u64>,
    terminal_at_ms: Option<u64>,
    /// Bounded tail of the child's recent output captured once at the
    /// terminal transition. Absent when capture was not eligible or failed;
    /// skipped on the wire so older runtimes keep loading stored records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_excerpt: Option<String>,
    /// Supervisor hold fact: how many non-terminal child tasks still named this
    /// task as their `parent_target` when its own turn settled. Present only
    /// while the hold is active; the task turns terminal on its next settle
    /// after those children are terminal. Absent on every other record, so
    /// older runtimes keep loading stored records unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    awaiting_children: Option<u32>,
    parent_target: Option<String>,
    #[serde(default)]
    parent_session_ref: Option<String>,
    /// EventCache boot epoch at registration time. Cross-boot reconcile
    /// cannot trust the persisted cursor against a new process's cache:
    /// fail closed instead of correlating unrelated later events.
    #[serde(default)]
    boot_epoch: Option<String>,
    notification_state: String,
    #[serde(default)]
    notification_attempts: u32,
    #[serde(default)]
    notification_at_ms: Option<u64>,
    #[serde(default)]
    notification_error: Option<String>,
    acknowledged_at_ms: Option<u64>,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_heartbeat_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_done_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_done_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stall_after_ms: Option<u64>,
}

impl PromptTaskRecord {
    fn is_terminal(&self) -> bool {
        self.lifecycle.is_terminal()
    }

    fn liveness(&self, now: u64) -> &'static str {
        if !self.is_terminal()
            && now.saturating_sub(self.updated_at_ms)
                >= self.stall_after_ms.unwrap_or(ESCALATION_STALL_IDLE_MS)
        {
            "stalled"
        } else {
            "active"
        }
    }

    fn public_json_at(&self, now: u64) -> Value {
        let mut public = json!({
            "task_id": self.task_id,
            "dispatch_id": self.dispatch_id,
            "target": self.target,
            "pane_id": self.pane_id,
            "workspace_id": self.workspace_id,
            "lifecycle": self.lifecycle.as_str(),
            "liveness": self.liveness(now),
            "activity_observed": self.activity_observed,
            "prior_settle_observed": self.prior_settle_observed,
            "turn_id": self.turn_id,
            "turn_correlation": if self.activity_observed {
                "runtime_activity_gate"
            } else if self.turn_id.is_some() {
                "runtime_dispatch"
            } else {
                "pending"
            },
            "exact_native_turn": self.exact_native_turn,
            "terminal": self.is_terminal(),
            "terminal_status": self.terminal_status,
            "terminal_cursor": self.terminal_cursor,
            "terminal_at_ms": self.terminal_at_ms,
            "terminal_seq": self.terminal_cursor,
            "terminal_event_id": self.terminal_cursor.map(|cursor| {
                format!("terminal:{}:{cursor}", self.task_id)
            }),
            "parent_target": self.parent_target,
            "parent_session_ref": self.parent_session_ref,
            "notification_state": self.notification_state,
            "notification_attempts": self.notification_attempts,
            "notification_at_ms": self.notification_at_ms,
            "notification_error": self.notification_error,
            "acknowledged": self.acknowledged_at_ms.is_some(),
            "created_at_ms": self.created_at_ms,
            "updated_at_ms": self.updated_at_ms,
        });
        if let Some(object) = public.as_object_mut() {
            for (key, value) in [
                ("last_heartbeat_at_ms", self.last_heartbeat_at_ms),
                ("worker_done_at_ms", self.worker_done_at_ms),
                ("stall_after_ms", self.stall_after_ms),
            ] {
                if let Some(value) = value {
                    object.insert(key.to_owned(), json!(value));
                }
            }
            if let Some(summary) = &self.worker_done_summary {
                object.insert("worker_done_summary".to_owned(), json!(summary));
            }
            if let Some(excerpt) = &self.final_excerpt {
                object.insert("final_excerpt".to_owned(), json!(excerpt));
            }
            if let Some(awaiting_children) = self.awaiting_children {
                object.insert("awaiting_children".to_owned(), json!(awaiting_children));
            }
        }
        public
    }
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

struct HandoffInjection {
    text: String,
    marked_now: bool,
}

impl HandoffInjection {
    fn not_marked(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            marked_now: false,
        }
    }

    fn marked(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            marked_now: true,
        }
    }
}

/// True only when the result proves the prompt never reached the agent
/// (pre-dispatch resolve failure or a pre-submission socket error). Success
/// and uncertain delivery (`delivery_uncertain`) both return false: the pane
/// keeps its handoff mark so we never risk a duplicate injection.
fn dispatch_definitely_not_delivered(result: &Value) -> bool {
    result.get("ok").and_then(Value::as_bool) == Some(false)
        && result.get("delivery_uncertain").and_then(Value::as_bool) != Some(true)
        && (result.get("delivery_state").and_then(Value::as_str) == Some("not_delivered")
            || result.get("retryable").and_then(Value::as_bool) == Some(true))
}

#[derive(Default)]
struct PromptRegistryInner {
    // Serialize durable task read/modify/write across report, lifecycle, ack and
    // parent notification so a stale snapshot cannot erase an accepted report.
    task_mutations: Mutex<()>,
    #[cfg(test)]
    clock: Option<Arc<AtomicU64>>,
    records: Mutex<HashMap<String, PromptRecord>>,
    handoff_injected_panes: Mutex<HashSet<String>>,
    store: Option<Arc<Mutex<StateStore>>>,
    cache: Option<Arc<EventCache>>,
    client: Option<HerdrClient>,
}

#[derive(Clone, Default)]
pub struct PromptRegistry {
    inner: Arc<PromptRegistryInner>,
}

#[derive(Debug)]
enum BeginPrompt {
    Reserved(Option<String>),
    Replay(Value),
}

impl PromptRegistry {
    fn now_ms(&self) -> u64 {
        #[cfg(test)]
        if let Some(clock) = &self.inner.clock {
            return clock.load(Ordering::SeqCst);
        }
        now_ms()
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<Mutex<StateStore>>) -> Self {
        Self {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: None,
                client: None,
            }),
        }
    }

    pub fn with_runtime(
        store: Arc<Mutex<StateStore>>,
        cache: Arc<EventCache>,
        client: HerdrClient,
    ) -> Self {
        let registry = Self {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: Some(cache),
                client: Some(client),
            }),
        };
        registry.reconcile_active_tasks();
        registry.start_task_tracker();
        registry
    }

    fn begin(&self, key: &str, fingerprint: &str) -> Result<BeginPrompt, Value> {
        if let Some(store) = &self.inner.store {
            return begin_persisted(store, key, fingerprint);
        }
        self.begin_memory(key, fingerprint)
    }

    fn begin_memory(&self, key: &str, fingerprint: &str) -> Result<BeginPrompt, Value> {
        let now = self.now_ms();
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
                    "hint": "A matching submission is already in progress; repeating it can duplicate work. The existing result or live agent state identifies the outcome.",
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

    /// Outcome of a first-dispatch handoff injection attempt. `marked_now` is
    /// true only when this call placed the per-pane mark, so a clearly failed
    /// dispatch can un-mark exactly the mark it created.
    fn inject_handoff_once(&self, pane_id: &str, worktree: &str, text: &str) -> HandoffInjection {
        let Ok(mut injected) = self.inner.handoff_injected_panes.lock() else {
            return HandoffInjection::not_marked(text);
        };
        if !injected.insert(pane_id.to_owned()) {
            return HandoffInjection::not_marked(text);
        }
        let Some(store) = &self.inner.store else {
            return HandoffInjection::marked(text);
        };
        let Ok(store) = store.lock() else {
            return HandoffInjection::marked(text);
        };
        let Ok(states) = store.work_states() else {
            return HandoffInjection::marked(text);
        };
        let Some(state) = states
            .into_iter()
            .filter(|state| state.worktree == worktree && !state.handoff_md.trim().is_empty())
            .max_by_key(|state| state.updated_at)
        else {
            return HandoffInjection::marked(text);
        };
        let handoff = crate::state_store::work_journal::clip_chars(&state.handoff_md, 16_000);
        HandoffInjection {
            text: format!("## Handoff context\n\n{handoff}\n\n---\n\n{text}"),
            marked_now: true,
        }
    }

    /// Release the per-pane injection mark after a dispatch that provably
    /// never reached the agent, so the next successful first dispatch still
    /// delivers the handoff. Never called for uncertain delivery.
    fn unmark_handoff_injected(&self, pane_id: &str) {
        if let Ok(mut injected) = self.inner.handoff_injected_panes.lock() {
            injected.remove(pane_id);
        }
    }

    fn pane_worktree(&self, pane_id: &str) -> Option<String> {
        let snapshot = self.inner.cache.as_ref()?.snapshot();
        pane_worktree_from_snapshot(&snapshot, pane_id, self.workspace_for_pane(pane_id))
    }

    fn current_task_cursor(&self) -> u64 {
        self.inner
            .cache
            .as_ref()
            .map(|cache| cache.digest_since(u64::MAX).cursor)
            .unwrap_or(0)
    }

    fn register_task(
        &self,
        input: RegisterTaskInput<'_>,
    ) -> Result<Option<PromptTaskRecord>, String> {
        let RegisterTaskInput {
            task_id,
            target,
            fingerprint,
            idempotency_key,
            before,
            after,
            submitted,
            start_cursor,
            parent_target,
            parent_session_ref,
            stall_after_ms,
        } = input;
        if submitted == Some(false) || self.inner.store.is_none() {
            return Ok(None);
        }
        let guard = self
            .inner
            .task_mutations
            .lock()
            .map_err(|_| "agent task mutation lock unavailable".to_owned())?;
        if let Some(existing) = self.load_task(task_id)? {
            return Ok(Some(existing));
        }

        let now = self.now_ms();
        let pane_id = after
            .and_then(|state| state.pane_id.clone())
            .or_else(|| before.and_then(|state| state.pane_id.clone()));
        let workspace_id = pane_id
            .as_deref()
            .and_then(|pane_id| self.workspace_for_pane(pane_id));
        let baseline_status = before.and_then(|state| state.agent_status.clone());
        let baseline_seq = before.and_then(|state| state.state_change_seq);
        let mut lifecycle = if baseline_status.as_deref() == Some("working") {
            PromptTaskLifecycle::AwaitingPriorSettle
        } else if submitted.is_none() {
            PromptTaskLifecycle::DeliveryUncertain
        } else {
            PromptTaskLifecycle::AwaitingActivity
        };
        let mut activity_observed = false;
        // This is the Runtime-owned turn envelope for this submission. It is
        // stable immediately, while exact_native_turn remains false until Herdr
        // exposes a native prompt-turn identity.
        let mut turn_id = Some(runtime_turn_id(task_id, "dispatch"));
        let after_status = after.and_then(|state| state.agent_status.as_deref());
        let after_seq = after.and_then(|state| state.state_change_seq);
        let seq_advanced = baseline_seq
            .zip(after_seq)
            .is_some_and(|(before, after)| before != after);
        if baseline_status.as_deref() != Some("working")
            && (after_status == Some("working") || seq_advanced)
        {
            lifecycle = PromptTaskLifecycle::Running;
            activity_observed = true;
            turn_id = Some(runtime_turn_id(
                task_id,
                &format!("seq:{}", after_seq.unwrap_or_default()),
            ));
        }

        let mut record = PromptTaskRecord {
            schema_version: 1,
            task_id: task_id.to_owned(),
            dispatch_id: task_id.to_owned(),
            target: target.to_owned(),
            idempotency_key_digest: idempotency_key
                .map(|key| stable_digest(&[b"herdr_prompt/task_key", key.as_bytes()])),
            pane_id,
            workspace_id,
            baseline_status,
            baseline_seq,
            start_cursor,
            last_cursor: start_cursor,
            lifecycle,
            activity_observed,
            prior_settle_observed: false,
            turn_id,
            exact_native_turn: false,
            terminal_status: None,
            terminal_cursor: None,
            terminal_at_ms: None,
            last_heartbeat_at_ms: None,
            worker_done_at_ms: None,
            worker_done_summary: None,
            stall_after_ms: None,
            final_excerpt: None,
            awaiting_children: None,
            parent_target: parent_target.map(str::to_owned),
            parent_session_ref: parent_session_ref.map(str::to_owned),
            boot_epoch: self
                .inner
                .cache
                .as_ref()
                .map(|cache| cache.boot_id().to_owned()),
            notification_state: "pending".to_owned(),
            notification_attempts: 0,
            notification_at_ms: None,
            notification_error: None,
            acknowledged_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        if record.lifecycle == PromptTaskLifecycle::DeliveryUncertain {
            mark_delivery_uncertain(&mut record, now, "delivery_state_unknown_at_submit");
        }
        record.stall_after_ms = stall_after_ms;
        self.save_task(&record, Some(fingerprint))?;
        drop(guard);
        let reconciled = self.reconcile_task(task_id)?;
        Ok(reconciled.or(Some(record)))
    }

    fn task_report(&self, params: &Value) -> Value {
        let task_id = match required_str(params, "task_id") {
            Ok(value) if !value.is_empty() && value.len() <= 128 => value,
            _ => return invalid("task_id must be a non-empty string up to 128 bytes"),
        };
        let dispatch_id = match required_str(params, "dispatch_id") {
            Ok(value) if !value.is_empty() && value.len() <= 128 => value,
            _ => return invalid("dispatch_id must be a non-empty string up to 128 bytes"),
        };
        let kind = match required_str(params, "kind") {
            Ok("heartbeat") => "heartbeat",
            Ok("worker_done") => "worker_done",
            _ => return invalid("kind must be heartbeat or worker_done"),
        };
        let summary = match optional_str(params, "summary") {
            Ok(value) => value.map(|text| {
                text.chars()
                    .filter(|ch| !ch.is_control())
                    .collect::<String>()
            }),
            Err(error) => return error,
        };
        if summary.as_ref().is_some_and(|text| text.len() > 2048) {
            return invalid(
                "summary must be at most 2048 bytes after stripping control characters",
            );
        }
        let Ok(guard) = self.inner.task_mutations.lock() else {
            return json!({"ok": false, "code": "agent_task_store_unavailable"});
        };
        let mut task = match self.load_task(task_id) {
            Ok(Some(task)) if task.task_id == task_id && task.dispatch_id == dispatch_id => task,
            Ok(_) => return json!({"ok": false, "code": "task_identity_mismatch"}),
            Err(error) => {
                return json!({"ok": false, "code": "agent_task_store_unavailable", "message": error});
            }
        };
        if kind == "worker_done" && task.worker_done_at_ms.is_some() {
            return json!({"ok": false, "code": "worker_done_already_reported"});
        }
        if kind == "heartbeat" && task.is_terminal() {
            return json!({"ok": false, "code": "agent_task_terminal"});
        }
        let now = self.now_ms();
        let newly_completed = kind == "worker_done" && !task.is_terminal();
        if kind == "heartbeat" {
            task.last_heartbeat_at_ms = Some(now);
        } else {
            task.worker_done_at_ms = Some(now);
            task.worker_done_summary = summary;
            if newly_completed {
                task.lifecycle = PromptTaskLifecycle::Completed;
                task.terminal_status = Some("worker_done".to_owned());
                task.terminal_at_ms = Some(now);
                task.terminal_cursor = None;
                task.awaiting_children = None;
                task.notification_state = "pending".to_owned();
            }
        }
        task.updated_at_ms = now;
        if let Err(error) = self.save_task(&task, None) {
            return json!({"ok": false, "code": "agent_task_store_unavailable", "message": error});
        }
        drop(guard);
        if newly_completed {
            self.maybe_notify_parent(&task);
        }
        json!({"ok": true, "task": task.public_json_at(now)})
    }

    pub fn task_status(&self, task_id: &str) -> Value {
        match self.reconcile_task(task_id) {
            Ok(Some(task)) => json!({"ok": true, "task": task.public_json_at(self.now_ms())}),
            Ok(None) => json!({
                "ok": false,
                "code": "agent_task_not_found",
                "task_id": task_id,
            }),
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
                "task_id": task_id,
            }),
        }
    }

    pub fn task_status_by_idempotency(
        &self,
        idempotency_key: &str,
        target: &str,
        parent_session_ref: Option<&str>,
    ) -> Value {
        let digest = stable_digest(&[b"herdr_prompt/task_key", idempotency_key.as_bytes()]);
        let Some(store) = self.inner.store.as_ref() else {
            return json!({"ok": false, "code": "agent_task_store_unavailable"});
        };
        let ids = match store.lock() {
            Ok(store) => store.task_operation_ids_by_key_digest(
                PROMPT_TASK_OPERATION_KIND,
                &digest,
                i64::try_from(now_ms()).unwrap_or(i64::MAX),
            ),
            Err(_) => Err("agent task store lock unavailable".to_owned()),
        };
        let ids = match ids {
            Ok(ids) => ids,
            Err(error) => {
                return json!({
                    "ok": false, "code": "agent_task_store_unavailable", "message": error,
                });
            }
        };
        if ids.len() > 1 {
            return json!({"ok": false, "code": "agent_task_lookup_ambiguous"});
        }
        let Some(task_id) = ids.first() else {
            return json!({"ok": false, "code": "agent_task_not_found"});
        };
        match self.reconcile_task(task_id) {
            Ok(Some(task))
                if task.idempotency_key_digest.as_deref() == Some(digest.as_str())
                    && task.target == target
                    && task.parent_session_ref.as_deref() == parent_session_ref =>
            {
                json!({"ok": true, "task": task.public_json_at(self.now_ms())})
            }
            Ok(_) => json!({"ok": false, "code": "agent_task_not_found"}),
            Err(error) => json!({
                "ok": false, "code": "agent_task_store_unavailable", "message": error,
            }),
        }
    }

    /// Read-only head check for one dispatch idempotency key.
    ///
    /// True when this call cannot produce a new submission because the runtime
    /// already knows the key: a completed replay payload, an in-flight
    /// reservation, a conflicting reuse, or a durable task that still owns the
    /// key (the same conditions `begin` resolves without reserving). Callers
    /// use it to keep a replayed dispatch a pure replay. It never reserves,
    /// writes, or prunes anything.
    pub fn dispatch_key_already_known(&self, idempotency_key: &str) -> bool {
        let now = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        let key_hash = stable_digest(&[b"herdr_prompt/idempotency", idempotency_key.as_bytes()]);
        let op_id = format!("op:prompt:{}", &key_hash[..32]);
        let Some(store) = self.inner.store.as_ref() else {
            return self
                .inner
                .records
                .lock()
                .map(|records| records.contains_key(idempotency_key))
                .unwrap_or(false);
        };
        let Ok(store) = store.lock() else {
            return false;
        };
        let live_operation = store
            .operation_ledger(&op_id)
            .ok()
            .flatten()
            .is_some_and(|record| {
                record.kind == PROMPT_OPERATION_KIND
                    && record.idempotency_key.as_deref() == Some(key_hash.as_str())
                    && record.expires_at.is_none_or(|expires_at| expires_at > now)
            });
        if live_operation {
            return true;
        }
        // The short replay reservation expires before the durable task does;
        // `begin` still refuses a new submission while a task owns the key.
        let digest = stable_digest(&[b"herdr_prompt/task_key", idempotency_key.as_bytes()]);
        store
            .task_operation_ids_by_key_digest(PROMPT_TASK_OPERATION_KIND, &digest, now)
            .map(|ids| !ids.is_empty())
            .unwrap_or(false)
    }

    pub fn task_status_for_parent_session(&self, task_id: &str, parent_session_ref: &str) -> Value {
        match self.reconcile_task(task_id) {
            Ok(Some(task)) if task.parent_session_ref.as_deref() == Some(parent_session_ref) => {
                json!({"ok": true, "task": task.public_json_at(self.now_ms())})
            }
            Ok(Some(_)) => json!({
                "ok": false,
                "code": "agent_task_parent_mismatch",
                "task_id": task_id,
            }),
            Ok(None) => json!({
                "ok": false,
                "code": "agent_task_not_found",
                "task_id": task_id,
            }),
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
                "task_id": task_id,
            }),
        }
    }

    pub fn task_inbox(
        &self,
        workspace_id: Option<&str>,
        parent_target: Option<&str>,
        parent_session_ref: Option<&str>,
        include_acknowledged: bool,
        advisory: bool,
        limit: usize,
    ) -> Value {
        let query = InboxQuery {
            workspace_id,
            parent_target,
            parent_session_ref,
            include_acknowledged,
            advisory,
            limit,
        };
        match self.inbox_payload(&query) {
            Ok(payload) => payload,
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
            }),
        }
    }

    /// Extension/planner inbox read that may also project the session's
    /// unfinished tasks.
    ///
    /// The default inbox lists terminal tasks only: a conversation is not woken
    /// by unfinished work. A conversation binding, however, has to survive a
    /// long-running supervisor task, so the extension may opt in to one compact
    /// row per non-terminal task of its own session. Those rows carry no
    /// excerpt, are never acked, and share the caller's `limit` with the
    /// terminal rows, which keep their wake priority.
    pub fn task_inbox_with_running(
        &self,
        workspace_id: Option<&str>,
        parent_session_ref: &str,
        limit: usize,
    ) -> Value {
        let mut payload = self.task_inbox(
            workspace_id,
            None,
            Some(parent_session_ref),
            false,
            true,
            limit,
        );
        if payload.get("ok").and_then(Value::as_bool) != Some(true) {
            return payload;
        }
        let Ok(tasks) = self.list_tasks(512) else {
            return payload;
        };
        let mut running = tasks
            .into_iter()
            .filter(|task| {
                !task.is_terminal()
                    && task.parent_session_ref.as_deref() == Some(parent_session_ref)
                    && workspace_id.is_none_or(|workspace_id| {
                        task.workspace_id.as_deref() == Some(workspace_id)
                    })
            })
            .collect::<Vec<_>>();
        running.sort_by_key(|task| std::cmp::Reverse(task.updated_at_ms));
        let Some(rows) = payload.get_mut("tasks").and_then(Value::as_array_mut) else {
            return payload;
        };
        let room = limit.saturating_sub(rows.len());
        rows.extend(
            running
                .into_iter()
                .take(room)
                .map(|task| running_task_row(task, self.now_ms())),
        );
        payload
    }

    /// Bounded inbox long-poll: returns immediately when an unacknowledged
    /// terminal task is already in scope, otherwise reconciles the durable task
    /// facts until one appears or `wait_ms` elapses. The returned payload is the
    /// normal inbox shape plus one `wait` object.
    fn task_inbox_waiting(&self, query: &InboxQuery, wait_ms: u64) -> Value {
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(wait_ms);
        let woke = loop {
            match self.inbox_has_pending_terminal(query) {
                Ok(true) => break "terminal_task",
                Ok(false) => {}
                Err(error) => {
                    return json!({
                        "ok": false,
                        "code": "agent_task_store_unavailable",
                        "message": error,
                    });
                }
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break "timeout";
            }
            thread::sleep(remaining.min(INBOX_WAIT_POLL));
        };
        let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut payload = match self.inbox_payload(query) {
            Ok(payload) => payload,
            Err(error) => {
                return json!({
                    "ok": false,
                    "code": "agent_task_store_unavailable",
                    "message": error,
                });
            }
        };
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "wait".to_owned(),
                json!({"waited_ms": waited_ms, "woke": woke}),
            );
        }
        payload
    }

    /// Wake predicate for the inbox long-poll: an unacknowledged terminal task
    /// inside the caller's scope. `include_acknowledged` deliberately does not
    /// relax it; the returned payload still honors the caller's own filter.
    ///
    /// Only the caller's own nonterminal tasks are reconciled here. The poll
    /// path must not sweep every lane's already-terminal notification
    /// bookkeeping, because that would run parent probes and attention advice
    /// on each tick instead of only on the returned snapshot.
    fn inbox_has_pending_terminal(&self, query: &InboxQuery) -> Result<bool, String> {
        let in_scope = |task: &PromptTaskRecord| {
            task_matches_scope(
                task,
                query.workspace_id,
                query.parent_target,
                query.parent_session_ref,
            )
        };
        let pending = |task: &PromptTaskRecord| {
            in_scope(task) && task.is_terminal() && task.acknowledged_at_ms.is_none()
        };
        let tasks = self.list_tasks(512)?;
        if tasks.iter().any(pending) {
            return Ok(true);
        }
        let reconcilable = tasks
            .iter()
            .filter(|task| in_scope(task) && !task.is_terminal())
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        if reconcilable.is_empty() {
            return Ok(false);
        }
        for task_id in &reconcilable {
            self.reconcile_task(task_id)?;
        }
        Ok(self.list_tasks(512)?.iter().any(pending))
    }

    fn inbox_payload(&self, query: &InboxQuery) -> Result<Value, String> {
        let InboxQuery {
            workspace_id,
            parent_target,
            parent_session_ref,
            include_acknowledged,
            advisory,
            limit,
        } = *query;
        self.reconcile_active_tasks();
        let tasks = self.list_tasks(512)?;
        let scoped = tasks
            .iter()
            .filter(|task| {
                task_matches_scope(task, workspace_id, parent_target, parent_session_ref)
            })
            .filter(|task| {
                !task.is_terminal() || include_acknowledged || task.acknowledged_at_ms.is_none()
            })
            .collect::<Vec<_>>();

        let mut terminal = scoped
            .iter()
            .copied()
            .filter(|task| task.is_terminal())
            .cloned()
            .collect::<Vec<_>>();
        terminal.sort_by(|left, right| {
            task_notification_priority(right)
                .cmp(&task_notification_priority(left))
                .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
        });
        terminal.truncate(limit.clamp(1, 512));

        let mut running = 0_u64;
        let mut completed = 0_u64;
        let mut blocked = 0_u64;
        let mut failed = 0_u64;
        let mut uncertain = 0_u64;
        for task in &scoped {
            match task.lifecycle {
                PromptTaskLifecycle::Running
                | PromptTaskLifecycle::AwaitingActivity
                | PromptTaskLifecycle::AwaitingPriorSettle => running += 1,
                PromptTaskLifecycle::DeliveryUncertain => uncertain += 1,
                PromptTaskLifecycle::Completed => completed += 1,
                PromptTaskLifecycle::Blocked => blocked += 1,
                PromptTaskLifecycle::Failed => failed += 1,
            }
        }

        let attention = if advisory && !terminal.is_empty() {
            task_attention_advice(scoped.iter().copied())
        } else {
            json!({
                "ok": true,
                "attempted": false,
                "used": false,
                "reason": if advisory { "no_pending_terminal" } else { "not_requested" },
                "advisory_only": true,
                "children": [],
            })
        };

        // Deterministic escalation facts use the scope filter only
        // (acknowledged rows still count) and are never gated on the
        // semantic `advisory` opt-in.
        let escalation = task_escalation(
            tasks.iter().filter(|task| {
                task_matches_scope(task, workspace_id, parent_target, parent_session_ref)
            }),
            self.now_ms(),
        );

        Ok(json!({
            "ok": true,
            "summary": {
                "running": running,
                "completed": completed,
                "blocked": blocked,
                "failed": failed,
                "uncertain": uncertain,
            },
            "tasks": terminal.into_iter().map(|task| task.public_json_at(self.now_ms())).collect::<Vec<_>>(),
            "attention": attention,
            "escalation": escalation,
        }))
    }

    pub fn task_ack_for_parent_session(&self, task_id: &str, parent_session_ref: &str) -> Value {
        match self.load_task(task_id) {
            Ok(Some(task)) if task.parent_session_ref.as_deref() != Some(parent_session_ref) => {
                json!({
                    "ok": false,
                    "code": "agent_task_parent_mismatch",
                    "task_id": task_id,
                })
            }
            Ok(_) => self.task_ack(task_id),
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
            }),
        }
    }

    pub fn task_ack(&self, task_id: &str) -> Value {
        let Ok(_guard) = self.inner.task_mutations.lock() else {
            return json!({"ok": false, "code": "agent_task_store_unavailable"});
        };
        match self.load_task(task_id) {
            Ok(Some(mut task)) => {
                let now = self.now_ms();
                task.acknowledged_at_ms = Some(now);
                task.notification_state = "acknowledged".to_owned();
                task.updated_at_ms = now;
                match self.save_task(&task, None) {
                    Ok(()) => json!({"ok": true, "task": task.public_json_at(self.now_ms())}),
                    Err(error) => json!({
                        "ok": false,
                        "code": "agent_task_store_unavailable",
                        "message": error,
                    }),
                }
            }
            Ok(None) => json!({
                "ok": false,
                "code": "agent_task_not_found",
                "task_id": task_id,
            }),
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
            }),
        }
    }

    /// Batch ack. Ids are processed in input order; unknown ids and ids owned
    /// by another parent session are reported as `not_found` and never acked.
    fn task_ack_many(&self, task_ids: &[&str], parent_session_ref: Option<&str>) -> Value {
        let mut acked = Vec::with_capacity(task_ids.len());
        let mut not_found = Vec::new();
        for task_id in task_ids {
            let outcome = match parent_session_ref {
                Some(parent_session_ref) => {
                    self.task_ack_for_parent_session(task_id, parent_session_ref)
                }
                None => self.task_ack(task_id),
            };
            match outcome.get("ok").and_then(Value::as_bool) {
                Some(true) => acked.push(*task_id),
                Some(false) => {
                    if matches!(
                        outcome.get("code").and_then(Value::as_str),
                        Some("agent_task_not_found" | "agent_task_parent_mismatch")
                    ) {
                        not_found.push(*task_id);
                    } else {
                        return outcome;
                    }
                }
                None => return outcome,
            }
        }
        json!({"ok": true, "acked": acked, "not_found": not_found})
    }

    fn wait_task(&self, task_id: &str, wait: &Value) -> Value {
        let timeout_ms = wait
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);
        let until = wait
            .get("until")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            match self.reconcile_task(task_id) {
                Ok(Some(task)) => {
                    if task_matches_wait(&task, &until) {
                        return json!({
                            "completed": true,
                            "task_id": task_id,
                            "matched": task.lifecycle.as_str(),
                            "task": task.public_json_at(self.now_ms()),
                        });
                    }
                    if task.is_terminal() {
                        return json!({
                            "completed": true,
                            "task_id": task_id,
                            "matched": task.lifecycle.as_str(),
                            "task": task.public_json_at(self.now_ms()),
                        });
                    }
                    if std::time::Instant::now() >= deadline {
                        return json!({
                            "completed": false,
                            "task_id": task_id,
                            "outcome": "checkpoint",
                            "task": task.public_json_at(self.now_ms()),
                        });
                    }
                }
                Ok(None) => {
                    return json!({
                        "completed": false,
                        "task_id": task_id,
                        "reason": "agent_task_not_found",
                    });
                }
                Err(error) => {
                    return json!({
                        "completed": false,
                        "task_id": task_id,
                        "reason": "agent_task_store_unavailable",
                        "message": error,
                    });
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn start_task_tracker(&self) {
        let Some(cache) = self.inner.cache.as_ref() else {
            return;
        };
        let mut cursor = cache.subscribe_cursor();
        let weak = Arc::downgrade(&self.inner);
        let _ = thread::Builder::new()
            .name("herdr-mcp-agent-task-tracker".to_owned())
            .spawn(move || {
                let mut last_timeout_scan = std::time::Instant::now();
                loop {
                    match cursor.has_changed() {
                        Ok(true) => {
                            let _ = cursor.borrow_and_update();
                            let Some(inner) = weak.upgrade() else {
                                return;
                            };
                            let registry = PromptRegistry { inner };
                            registry.reconcile_active_tasks();
                        }
                        Ok(false) => thread::sleep(Duration::from_millis(50)),
                        Err(_) => return,
                    }
                    if last_timeout_scan.elapsed() >= Duration::from_secs(1) {
                        let Some(inner) = weak.upgrade() else {
                            return;
                        };
                        let registry = PromptRegistry { inner };
                        registry.reconcile_activity_timeouts();
                        last_timeout_scan = std::time::Instant::now();
                    }
                }
            });
    }

    fn reconcile_active_tasks(&self) {
        let Ok(tasks) = self.list_tasks(512) else {
            return;
        };
        for task in tasks {
            if task.is_terminal() {
                if task.notification_state == "pending" {
                    self.maybe_notify_parent(&task);
                }
            } else {
                let _ = self.reconcile_task(&task.task_id);
            }
        }
    }

    fn reconcile_activity_timeouts(&self) {
        let Ok(tasks) = self.list_tasks(512) else {
            return;
        };
        for task in tasks {
            if task.lifecycle == PromptTaskLifecycle::AwaitingActivity {
                let _ = self.reconcile_task(&task.task_id);
            }
        }
    }

    fn maybe_notify_parent(&self, trigger: &PromptTaskRecord) {
        let Ok(_guard) = self.inner.task_mutations.lock() else {
            return;
        };
        self.maybe_notify_parent_locked(trigger);
    }

    fn maybe_notify_parent_locked(&self, trigger: &PromptTaskRecord) {
        let Some(parent_target) = trigger.parent_target.as_deref() else {
            return;
        };
        if trigger.notification_state != "pending" {
            return;
        }
        let Some(client) = self.inner.client.as_ref() else {
            return;
        };
        let Some(parent_state) = agent_state_of(client, parent_target) else {
            return;
        };
        if !matches!(parent_state.agent_status.as_deref(), Some("idle" | "done")) {
            return;
        }

        let Ok(all_tasks) = self.list_tasks(512) else {
            return;
        };
        let attention = task_attention_advice(all_tasks.iter().filter(|task| {
            task.parent_target.as_deref() == Some(parent_target)
                && (!task.is_terminal() || task.notification_state == "pending")
        }));

        let mut pending = all_tasks
            .into_iter()
            .filter(|task| {
                task.parent_target.as_deref() == Some(parent_target)
                    && task.is_terminal()
                    && task.notification_state == "pending"
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        pending.sort_by(|left, right| {
            task_notification_priority(right)
                .cmp(&task_notification_priority(left))
                .then_with(|| {
                    left.terminal_cursor
                        .unwrap_or(u64::MAX)
                        .cmp(&right.terminal_cursor.unwrap_or(u64::MAX))
                })
        });
        pending.truncate(32);

        let now = self.now_ms();
        for task in &mut pending {
            task.notification_state = "in_flight".to_owned();
            task.notification_attempts = task.notification_attempts.saturating_add(1);
            task.notification_at_ms = Some(now);
            task.notification_error = None;
            task.updated_at_ms = now;
            if self.save_task(task, None).is_err() {
                return;
            }
        }

        let mut lines = vec![format!(
            "Herdr child-task update: {} terminal task(s).",
            pending.len()
        )];
        for task in &pending {
            let attention_state = task_attention_state(&attention, &task.task_id)
                .map(|state| format!(" [attention={state}]"))
                .unwrap_or_default();
            // The final excerpt deliberately stays out of the wake text: child
            // pane output must not be typed into the parent's prompt, and TUI
            // agents end their screen with chrome rather than the outcome.
            lines.push(format!(
                "- {}: {} (child {}, turn {}, terminal_seq {}){}",
                task.task_id,
                task.lifecycle.as_str(),
                task.target,
                task.turn_id.as_deref().unwrap_or("unverified"),
                task.terminal_cursor
                    .map(|cursor| cursor.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                attention_state
            ));
        }
        lines.push(
            "Attention is advisory. Use durable task facts as authority: verify_completion enters deterministic validation; continue_unobserved leaves independent running children alone. Do not re-dispatch a terminal or delivery-uncertain task blindly."
                .to_owned(),
        );
        let text = lines.join("\n");

        let outcome = client.call_with_timeout(
            "agent.prompt",
            json!({"target": parent_target, "text": text}),
            Duration::from_secs(10),
        );
        let finished_at = now_ms();
        match outcome {
            Ok(_) => {
                for task in &mut pending {
                    task.notification_state = "delivered".to_owned();
                    task.notification_at_ms = Some(finished_at);
                    task.notification_error = None;
                    task.updated_at_ms = finished_at;
                    let _ = self.save_task(task, None);
                }
            }
            Err(error) => {
                let definitely_not_delivered = definitely_not_delivered(&error);
                for task in &mut pending {
                    task.notification_state = if definitely_not_delivered {
                        "pending".to_owned()
                    } else {
                        "uncertain".to_owned()
                    };
                    task.notification_at_ms = Some(finished_at);
                    task.notification_error = Some(truncate_chars(&error.message, 512));
                    task.updated_at_ms = finished_at;
                    let _ = self.save_task(task, None);
                }
            }
        }
    }

    fn reconcile_task(&self, task_id: &str) -> Result<Option<PromptTaskRecord>, String> {
        let _guard = self
            .inner
            .task_mutations
            .lock()
            .map_err(|_| "agent task mutation lock unavailable".to_owned())?;
        self.reconcile_task_locked(task_id)
    }

    fn reconcile_task_locked(&self, task_id: &str) -> Result<Option<PromptTaskRecord>, String> {
        let Some(mut task) = self.load_task(task_id)? else {
            return Ok(None);
        };
        if task.is_terminal() {
            return Ok(Some(task));
        }
        let mut changed = false;
        if task.lifecycle == PromptTaskLifecycle::AwaitingActivity
            && self.now_ms().saturating_sub(task.updated_at_ms)
                >= PROMPT_TASK_ACTIVITY_START_TIMEOUT_MS
        {
            changed |= mark_delivery_uncertain(&mut task, self.now_ms(), "activity_start_timeout");
        }
        if task.is_terminal() {
            if changed {
                self.save_task(&task, None)?;
                self.maybe_notify_parent_locked(&task);
                if let Some(reloaded) = self.load_task(task_id)? {
                    task = reloaded;
                }
            }
            return Ok(Some(task));
        }
        let Some(cache) = self.inner.cache.as_ref() else {
            return Ok(Some(task));
        };
        // Boot-epoch fence: the persisted cursor is only meaningful against
        // the EventCache of the process that recorded it. A different (or
        // unknown, for pre-epoch records) boot means the cursor cannot be
        // trusted and later events from the new cache may belong to unrelated
        // conversations — fail closed, never optimistically complete.
        let boot_mismatch = match task.boot_epoch.as_deref() {
            Some(epoch) => epoch != cache.boot_id(),
            None => true, // legacy record without an epoch: fail closed
        };
        if boot_mismatch {
            let reason = if task.boot_epoch.is_some() {
                "runtime_boot_changed"
            } else {
                "runtime_boot_unknown"
            };
            changed |= mark_delivery_uncertain(&mut task, self.now_ms(), reason);
            if changed {
                self.save_task(&task, None)?;
                self.maybe_notify_parent_locked(&task);
                if let Some(reloaded) = self.load_task(task_id)? {
                    task = reloaded;
                }
            }
            return Ok(Some(task));
        }
        let digest = cache.digest_since(task.last_cursor);
        changed |= self.apply_task_window(&mut task, digest.cursor, &digest.events)?;
        if changed {
            self.save_task(&task, None)?;
            if task.is_terminal() {
                self.maybe_notify_parent_locked(&task);
                if let Some(reloaded) = self.load_task(task_id)? {
                    task = reloaded;
                }
            }
        }
        Ok(Some(task))
    }

    /// Applies one ordered cache window to a task record that is neither
    /// terminal nor fenced: event history gap detection, the supervisor settle
    /// hold, and cursor bookkeeping in the one place `reconcile_task` uses.
    ///
    /// A held settle still advances `last_cursor` past its own event, so the
    /// supervisor's later settle arrives as a strictly newer cursor and is
    /// applied normally once its children are terminal.
    fn apply_task_window(
        &self,
        task: &mut PromptTaskRecord,
        cursor: u64,
        events: &[Value],
    ) -> Result<bool, String> {
        let mut changed = if cursor.saturating_sub(task.last_cursor) > 2048 {
            mark_delivery_uncertain(task, now_ms(), "event_history_gap")
        } else {
            apply_events_with_final_excerpt(self.inner.client.as_ref(), Some(self), task, events)?
        };
        if cursor > task.last_cursor {
            task.last_cursor = cursor;
            changed = true;
        }
        Ok(changed)
    }

    /// Number of non-terminal tasks that still name this task as their parent.
    ///
    /// Dispatch records `parent_target` exactly as its dispatcher wrote it, so a
    /// child may name either the supervisor's target or its pane id; both are
    /// compared through `task_matches_target` just like the dispatch guards.
    fn nonterminal_children(&self, task: &PromptTaskRecord) -> Result<u32, String> {
        let children =
            self.list_tasks(512)?
                .into_iter()
                .filter(|child| {
                    child.task_id != task.task_id
                        && !child.is_terminal()
                        && child.parent_target.as_deref().is_some_and(|parent_target| {
                            task_matches_target(task, parent_target, None)
                        })
                })
                .count();
        Ok(u32::try_from(children).unwrap_or(u32::MAX))
    }

    fn workspace_for_pane(&self, pane_id: &str) -> Option<String> {
        let snapshot = self.inner.cache.as_ref()?.snapshot();
        snapshot
            .get("panes")
            .and_then(Value::as_array)
            .and_then(|panes| {
                panes
                    .iter()
                    .find(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(pane_id))
            })
            .and_then(|pane| pane.get("workspace_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                snapshot
                    .get("agents")
                    .and_then(Value::as_array)
                    .and_then(|agents| {
                        agents.iter().find(|agent| {
                            agent.get("pane_id").and_then(Value::as_str) == Some(pane_id)
                        })
                    })
                    .and_then(|agent| agent.get("workspace_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
    }

    fn load_task(&self, task_id: &str) -> Result<Option<PromptTaskRecord>, String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(None);
        };
        let store = store
            .lock()
            .map_err(|_| "agent task store lock unavailable".to_owned())?;
        let Some(record) = store.operation_ledger(task_id)? else {
            return Ok(None);
        };
        if record.kind != PROMPT_TASK_OPERATION_KIND {
            return Ok(None);
        }
        decode_task_record(&record).map(Some)
    }

    fn list_tasks(&self, limit: usize) -> Result<Vec<PromptTaskRecord>, String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(vec![]);
        };
        let store = store
            .lock()
            .map_err(|_| "agent task store lock unavailable".to_owned())?;
        store
            .operation_ledgers(PROMPT_TASK_OPERATION_KIND, limit)?
            .into_iter()
            .map(|record| decode_task_record(&record))
            .collect()
    }

    fn save_task(&self, task: &PromptTaskRecord, request_hash: Option<&str>) -> Result<(), String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(());
        };
        let encoded = serde_json::to_string(task)
            .map_err(|error| format!("cannot encode agent task: {error}"))?;
        let now = now_ms();
        let expires_at = now.saturating_add(PROMPT_TASK_TTL_MS);
        let mut store = store
            .lock()
            .map_err(|_| "agent task store lock unavailable".to_owned())?;
        store.put_operation_ledger(OperationLedgerInput {
            op_id: &task.task_id,
            kind: PROMPT_TASK_OPERATION_KIND,
            idempotency_key: task.idempotency_key_digest.as_deref(),
            request_hash,
            phase: if task.is_terminal() {
                "terminal"
            } else {
                "active"
            },
            state: task.lifecycle.as_str(),
            result_json: &encoded,
            now_ms: i64::try_from(now).unwrap_or(i64::MAX),
            expires_at: Some(i64::try_from(expires_at).unwrap_or(i64::MAX)),
        })
    }
}

fn mark_delivery_uncertain(task: &mut PromptTaskRecord, now_ms: u64, reason: &str) -> bool {
    if task.is_terminal() {
        return false;
    }
    task.lifecycle = PromptTaskLifecycle::DeliveryUncertain;
    task.terminal_status = Some("delivery_uncertain".to_owned());
    task.terminal_cursor = None;
    task.terminal_at_ms = Some(now_ms);
    task.notification_state = "pending".to_owned();
    task.notification_error = Some(reason.to_owned());
    task.updated_at_ms = now_ms;
    true
}

/// Scope and presentation inputs for one `agent.task.inbox` read.
#[derive(Debug, Clone, Copy)]
struct InboxQuery<'a> {
    workspace_id: Option<&'a str>,
    parent_target: Option<&'a str>,
    parent_session_ref: Option<&'a str>,
    include_acknowledged: bool,
    advisory: bool,
    limit: usize,
}

/// Inbox task projection: `full` keeps today's payload, `compact` drops the
/// fields a supervising planner does not need for a round trip.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum InboxView {
    Full,
    Compact,
}

/// Fields kept by `view: compact`. Attention facts are only carried when the
/// task object already exposes them; the raw `parent_session_ref` never is.
/// `final_excerpt` is present only when captured and spares the planner an
/// `agent.read` round trip; `awaiting_children` is present only while a
/// supervisor is held for its own children.
const COMPACT_TASK_FIELDS: [&str; 12] = [
    "task_id",
    "dispatch_id",
    "target",
    "pane_id",
    "lifecycle",
    "liveness",
    "terminal",
    "acknowledged",
    "terminal_at_ms",
    "updated_at_ms",
    "final_excerpt",
    "awaiting_children",
];

fn compact_task(task: Map<String, Value>) -> Map<String, Value> {
    task.into_iter()
        .filter(|(key, _)| {
            COMPACT_TASK_FIELDS.contains(&key.as_str()) || key.starts_with("attention")
        })
        .collect()
}

/// One non-terminal row for the opt-in extension inbox view: identity, lane
/// lifecycle, and the supervisor hold fact only. Deliberately no excerpt (the
/// task has no outcome yet) and no acknowledged state (it cannot be acked).
fn running_task_row(task: PromptTaskRecord, now: u64) -> Value {
    let mut row = json!({
        "task_id": task.task_id,
        "dispatch_id": task.dispatch_id,
        "workspace_id": task.workspace_id,
        "lifecycle": task.lifecycle.as_str(),
        "liveness": task.liveness(now),
        "terminal": false,
    });
    if let (Some(awaiting_children), Some(object)) = (task.awaiting_children, row.as_object_mut()) {
        object.insert("awaiting_children".to_owned(), json!(awaiting_children));
    }
    row
}

fn apply_inbox_view(payload: Value, view: InboxView) -> Value {
    if view == InboxView::Full {
        return payload;
    }
    let Value::Object(mut object) = payload else {
        return payload;
    };
    if let Some(Value::Array(tasks)) = object.get_mut("tasks") {
        for task in tasks.iter_mut() {
            if let Value::Object(task) = task {
                *task = compact_task(std::mem::take(task));
            }
        }
    }
    Value::Object(object)
}

fn parse_inbox_view(params: &Value) -> Result<InboxView, Value> {
    let Some(view) = params.get("view") else {
        return Ok(InboxView::Full);
    };
    match view.as_str() {
        Some("full") => Ok(InboxView::Full),
        Some("compact") => Ok(InboxView::Compact),
        _ => Err(invalid(r#"view must be "full" or "compact""#)),
    }
}

fn parse_inbox_wait_ms(params: &Value) -> Result<Option<u64>, Value> {
    let Some(wait_ms) = params.get("wait_ms") else {
        return Ok(None);
    };
    match wait_ms.as_u64() {
        Some(wait_ms) if (1..=MAX_WAIT_MS).contains(&wait_ms) => Ok(Some(wait_ms)),
        _ => Err(invalid("wait_ms must be an integer between 1 and 60000")),
    }
}

fn parse_task_ids(value: &Value) -> Result<Vec<String>, Value> {
    let Some(values) = value.as_array() else {
        return Err(invalid("task_ids must be an array of strings"));
    };
    if values.is_empty() || values.len() > MAX_TASK_ACK_BATCH {
        return Err(invalid("task_ids must contain between 1 and 32 ids"));
    }
    let mut seen = HashSet::new();
    let mut ids = Vec::with_capacity(values.len());
    for value in values {
        let Some(task_id) = value.as_str() else {
            return Err(invalid("task_ids must be an array of strings"));
        };
        if task_id.is_empty() || task_id.len() > 128 {
            return Err(invalid(
                "task_ids entries must be non-empty strings up to 128 bytes",
            ));
        }
        if !seen.insert(task_id) {
            return Err(invalid("task_ids must be unique"));
        }
        ids.push(task_id.to_owned());
    }
    Ok(ids)
}

fn task_matches_scope(
    task: &PromptTaskRecord,
    workspace_id: Option<&str>,
    parent_target: Option<&str>,
    parent_session_ref: Option<&str>,
) -> bool {
    workspace_id.is_none_or(|workspace_id| task.workspace_id.as_deref() == Some(workspace_id))
        && parent_target
            .is_none_or(|parent_target| task.parent_target.as_deref() == Some(parent_target))
        && parent_session_ref.is_none_or(|parent_session_ref| {
            task.parent_session_ref.as_deref() == Some(parent_session_ref)
        })
}

/// Resolves a dispatch target to the durable task it addresses: the same agent
/// may be named by its target string, by that exact pane id, or by a pane the
/// caller had to probe. Dispatch accepts all three forms, so every parent/child
/// comparison goes through this normalization instead of raw string equality.
fn task_matches_target(task: &PromptTaskRecord, target: &str, resolved_pane: Option<&str>) -> bool {
    task.target == target
        || task.pane_id.as_deref() == Some(target)
        || resolved_pane.is_some_and(|pane| task.pane_id.as_deref() == Some(pane))
}

fn task_notification_priority(task: &PromptTaskRecord) -> u32 {
    match task.lifecycle {
        PromptTaskLifecycle::Failed => 400,
        PromptTaskLifecycle::DeliveryUncertain => 375,
        PromptTaskLifecycle::Blocked => 350,
        PromptTaskLifecycle::Completed => 100,
        _ => 0,
    }
}

/// Deterministic escalation hints derived only from existing durable task
/// records.
///
/// Advisory only: no semantic call, no state mutation, and no automatic
/// re-dispatch (`redispatch` is fixed to `never_automatic`). Uncertainty is not
/// failure: `delivery_uncertain` neither extends an unsuccessful streak nor
/// produces a consult hint. The raw `parent_session_ref` value stays private;
/// callers only learn whether the lane is session scoped.
fn task_escalation<'a>(tasks: impl IntoIterator<Item = &'a PromptTaskRecord>, now: u64) -> Value {
    let mut lanes: HashMap<(&str, Option<&str>), Vec<&PromptTaskRecord>> = HashMap::new();
    for task in tasks {
        if now.saturating_sub(task.updated_at_ms) > ESCALATION_WINDOW_MS {
            continue;
        }
        lanes
            .entry((task.target.as_str(), task.parent_session_ref.as_deref()))
            .or_default()
            .push(task);
    }

    // Sort key keeps the externally specified (kind, then target) order and
    // adds internal lane/evidence tiebreaks so ordering stays total.
    let mut signals: Vec<((String, String, String, String), Value)> = Vec::new();
    for ((target, parent_ref), mut lane) in lanes {
        lane.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
                .then_with(|| right.task_id.cmp(&left.task_id))
        });
        let pane_id = lane[0].pane_id.as_deref();
        let lane_key = |kind: &str, task_id: &str| {
            (
                kind.to_owned(),
                target.to_owned(),
                parent_ref.unwrap_or_default().to_owned(),
                task_id.to_owned(),
            )
        };
        let recent_task_ids = |limit: usize| {
            lane.iter()
                .take(limit)
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>()
        };

        if lane.len() >= ESCALATION_REPAIR_BUDGET_TASKS {
            let mut signal = escalation_signal(
                "repair_budget_exhausted",
                "consult_advisor",
                target,
                pane_id,
                parent_ref,
            );
            signal.insert("dispatch_count".to_owned(), json!(lane.len()));
            signal.insert(
                "task_ids".to_owned(),
                json!(recent_task_ids(ESCALATION_MAX_TASK_IDS)),
            );
            signals.push((
                lane_key("repair_budget_exhausted", &lane[0].task_id),
                Value::Object(signal),
            ));
        }

        // Only terminal outcomes form the streak: a still-running dispatch is
        // not an unsuccessful round, and uncertainty is not a failure.
        let streak = lane
            .iter()
            .copied()
            .filter(|task| task.is_terminal())
            .take_while(|task| {
                matches!(
                    task.lifecycle,
                    PromptTaskLifecycle::Failed | PromptTaskLifecycle::Blocked
                )
            })
            .collect::<Vec<_>>();
        if streak.len() >= ESCALATION_UNSUCCESSFUL_STREAK {
            let mut signal = escalation_signal(
                "unsuccessful_streak",
                "consult_advisor",
                target,
                pane_id,
                parent_ref,
            );
            signal.insert("streak_length".to_owned(), json!(streak.len()));
            signal.insert(
                "task_ids".to_owned(),
                json!(
                    streak
                        .iter()
                        .take(ESCALATION_MAX_TASK_IDS)
                        .map(|task| task.task_id.clone())
                        .collect::<Vec<_>>()
                ),
            );
            signals.push((
                lane_key("unsuccessful_streak", &streak[0].task_id),
                Value::Object(signal),
            ));
        }

        for task in lane
            .iter()
            .copied()
            .filter(|task| task.liveness(now) == "stalled")
        {
            let idle_ms = now.saturating_sub(task.updated_at_ms);
            let mut signal = escalation_signal(
                "stall_suspected",
                "verify_progress",
                target,
                task.pane_id.as_deref(),
                parent_ref,
            );
            signal.insert("task_id".to_owned(), json!(task.task_id));
            signal.insert("lifecycle".to_owned(), json!(task.lifecycle.as_str()));
            signal.insert("idle_ms".to_owned(), json!(idle_ms));
            signals.push((
                lane_key("stall_suspected", &task.task_id),
                Value::Object(signal),
            ));
        }
    }

    signals.sort_by(|left, right| left.0.cmp(&right.0));
    let signals = signals
        .into_iter()
        .map(|(_, signal)| signal)
        .take(ESCALATION_MAX_SIGNALS)
        .collect::<Vec<_>>();
    json!({
        "advisory_only": true,
        "redispatch": "never_automatic",
        "signals": signals,
    })
}

fn escalation_signal(
    kind: &str,
    recommended_action: &str,
    target: &str,
    pane_id: Option<&str>,
    parent_session_ref: Option<&str>,
) -> Map<String, Value> {
    let mut signal = Map::new();
    signal.insert("kind".to_owned(), json!(kind));
    signal.insert("recommended_action".to_owned(), json!(recommended_action));
    signal.insert("target".to_owned(), json!(target));
    if let Some(pane_id) = pane_id {
        signal.insert("pane_id".to_owned(), json!(pane_id));
    }
    // Callers only learn whether the lane is session scoped, never the value.
    signal.insert(
        "parent_session_ref_present".to_owned(),
        json!(parent_session_ref.is_some()),
    );
    signal
}

fn task_attention_advice<'a>(tasks: impl IntoIterator<Item = &'a PromptTaskRecord>) -> Value {
    let mut tasks = tasks.into_iter().collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        task_notification_priority(right)
            .cmp(&task_notification_priority(left))
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
    });
    tasks.truncate(16);
    if tasks.is_empty() {
        return json!({
            "ok": true,
            "attempted": false,
            "used": false,
            "reason": "no_children",
            "advisory_only": true,
            "children": [],
        });
    }

    let observed_at = now_ms();
    let children = tasks
        .into_iter()
        .map(|task| {
            json!({
                "task_id": task.task_id,
                "agent_id": task.target,
                "terminal_state": task.lifecycle.as_str(),
                "age_ms": observed_at.saturating_sub(task.updated_at_ms),
            })
        })
        .collect::<Vec<_>>();
    agent_attention_advice(&json!({"children": children}))
}

fn task_attention_state(attention: &Value, task_id: &str) -> Option<String> {
    let child_id = attention
        .get("children")
        .and_then(Value::as_array)?
        .iter()
        .find(|child| child.get("task_id").and_then(Value::as_str) == Some(task_id))?
        .get("id")
        .and_then(Value::as_str)?;
    attention
        .get("assessments")
        .and_then(Value::as_array)?
        .iter()
        .find(|assessment| assessment.get("id").and_then(Value::as_str) == Some(child_id))?
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn decode_task_record(record: &OperationLedgerRecord) -> Result<PromptTaskRecord, String> {
    let encoded = record
        .result_json
        .as_deref()
        .ok_or_else(|| format!("agent task {} has no durable payload", record.op_id))?;
    serde_json::from_str(encoded)
        .map_err(|error| format!("cannot decode agent task {}: {error}", record.op_id))
}

fn runtime_turn_id(task_id: &str, marker: &str) -> String {
    let digest = stable_digest(&[
        b"herdr_prompt/runtime_turn",
        task_id.as_bytes(),
        marker.as_bytes(),
    ]);
    format!("turn:agent:{}", &digest[..32])
}

fn task_matches_wait(task: &PromptTaskRecord, until: &[String]) -> bool {
    if until.is_empty() {
        return task.is_terminal();
    }
    until.iter().any(|status| match status.as_str() {
        "working" => task.activity_observed,
        "blocked" => matches!(task.lifecycle, PromptTaskLifecycle::Blocked),
        "idle" | "done" => matches!(task.lifecycle, PromptTaskLifecycle::Completed),
        "unknown" => false,
        _ => false,
    })
}

fn task_event_status(event: &Value) -> Option<&str> {
    event
        .get("agent_status")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .get("pane")
                .and_then(|pane| pane.get("agent_status"))
                .and_then(Value::as_str)
        })
}

/// Normalizes both event shapes (`pane.closed` and `pane_closed`) to the same
/// underscore form so lifecycle handling never depends on the wire spelling.
fn task_event_type(event: &Value) -> String {
    event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .replace('.', "_")
}

fn event_matches_task_pane(task: &PromptTaskRecord, event: &Value) -> bool {
    let pane_id = event.get("pane_id").and_then(Value::as_str).or_else(|| {
        event
            .get("pane")
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
    });
    task.pane_id.as_deref() == pane_id
}

fn apply_task_event(task: &mut PromptTaskRecord, event: &Value) -> bool {
    let cursor = event.get("cursor").and_then(Value::as_u64).unwrap_or(0);
    if task.is_terminal() || cursor <= task.start_cursor || cursor <= task.last_cursor {
        return false;
    }
    if !event_matches_task_pane(task, event) {
        return false;
    }

    let kind = task_event_type(event);
    let now = now_ms();
    if matches!(kind.as_str(), "pane_closed" | "pane_exited") {
        task.lifecycle = PromptTaskLifecycle::Failed;
        task.terminal_status = Some(kind);
        task.terminal_cursor = Some(cursor);
        task.terminal_at_ms = Some(now);
        task.notification_state = "pending".to_owned();
        task.updated_at_ms = now;
        return true;
    }

    let Some(status) = task_event_status(event) else {
        return false;
    };
    if task.lifecycle == PromptTaskLifecycle::AwaitingPriorSettle {
        if matches!(status, "idle" | "done" | "blocked") {
            task.prior_settle_observed = true;
            task.lifecycle = PromptTaskLifecycle::AwaitingActivity;
            task.updated_at_ms = now;
            return true;
        }
        return false;
    }

    match status {
        "working" => {
            task.activity_observed = true;
            task.lifecycle = PromptTaskLifecycle::Running;
            if task.turn_id.is_none() {
                task.turn_id = Some(runtime_turn_id(
                    task.task_id.as_str(),
                    &format!("cursor:{cursor}"),
                ));
            }
            task.updated_at_ms = now;
            true
        }
        "blocked" => {
            if !task.activity_observed {
                task.activity_observed = true;
                if task.turn_id.is_none() {
                    task.turn_id = Some(runtime_turn_id(
                        task.task_id.as_str(),
                        &format!("cursor:{cursor}"),
                    ));
                }
            }
            task.lifecycle = PromptTaskLifecycle::Blocked;
            task.terminal_status = Some("blocked".to_owned());
            task.terminal_cursor = Some(cursor);
            task.terminal_at_ms = Some(now);
            task.notification_state = "pending".to_owned();
            task.updated_at_ms = now;
            true
        }
        "idle" | "done" if task.activity_observed => {
            task.lifecycle = PromptTaskLifecycle::Completed;
            task.terminal_status = Some(status.to_owned());
            task.terminal_cursor = Some(cursor);
            task.terminal_at_ms = Some(now);
            task.notification_state = "pending".to_owned();
            task.updated_at_ms = now;
            true
        }
        _ => false,
    }
}

/// Lines of recent unwrapped child output captured at the terminal transition.
const FINAL_EXCERPT_LINES: usize = 40;
/// Hard byte cap for the stored excerpt, measured from its end.
const FINAL_EXCERPT_MAX_BYTES: usize = 2_000;
/// The one-shot excerpt read shares the native socket path but keeps its own
/// short bound so a terminal reconcile never stalls on a slow agent read.
const FINAL_EXCERPT_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// If the pane's state cannot be revalidated promptly, omit the excerpt.
const FINAL_EXCERPT_VERIFY_TIMEOUT: Duration = Duration::from_secs(1);

/// Applies cache events to a durable task record, keeps a supervisor that
/// completed while its children still run non-terminal, and — exactly once at
/// an eligible terminal transition — captures the bounded final excerpt of the
/// child's recent output. A failed capture only leaves `final_excerpt` unset;
/// lifecycle, notification, and ack state are untouched, including the
/// fail-closed case where the terminal event carries no producer sequence.
///
/// `registry` is `Some` when the caller owns the durable task ledger and can
/// count this task's still-running children. `None` applies events only, for
/// callers with no ledger to consult. A failed child lookup returns an error
/// before anything is persisted, so the caller retries the same window instead
/// of settling a supervisor early.
fn apply_events_with_final_excerpt(
    client: Option<&HerdrClient>,
    registry: Option<&PromptRegistry>,
    task: &mut PromptTaskRecord,
    events: &[Value],
) -> Result<bool, String> {
    let mut changed = false;
    let mut terminal_state_change_seq = None;
    for event in events {
        let was_terminal = task.is_terminal();
        if apply_task_event(task, event) {
            changed = true;
            if !was_terminal && task.is_terminal() && final_excerpt_eligible(task) {
                terminal_state_change_seq = task_event_state_change_seq(event);
            }
        }
    }
    if changed {
        // Any newer activity supersedes the recorded hold fact.
        task.awaiting_children = None;
        if let Some(registry) = registry
            && matches!(
                task.lifecycle,
                PromptTaskLifecycle::Completed | PromptTaskLifecycle::Blocked
            )
        {
            let awaiting_children = registry.nonterminal_children(task)?;
            if awaiting_children > 0 {
                if task.lifecycle == PromptTaskLifecycle::Completed {
                    hold_settled_task_for_children(task, awaiting_children);
                } else {
                    // A blocked supervisor waits on a human or permission
                    // boundary. Holding it would hide that boundary and nothing
                    // would wake the parent again, so it surfaces immediately and
                    // keeps the outstanding child count only as a fact.
                    task.awaiting_children = Some(awaiting_children);
                }
            }
        }
    }
    // A held settle is no longer terminal, so it never reaches this qualified
    // capture: the excerpt fence and the supervisor hold cannot both apply.
    if changed
        && task.is_terminal()
        && task.final_excerpt.is_none()
        && final_excerpt_eligible(task)
        && !batch_proves_pane_advanced(task, events, terminal_state_change_seq)
    {
        task.final_excerpt = read_final_excerpt(client, task, terminal_state_change_seq);
    }
    Ok(changed)
}

/// Keeps a supervisor that completed while its own children still run
/// non-terminal. The settle event itself is already consumed, so the task stays
/// `running` with an explicit `awaiting_children` fact and turns terminal on its
/// next settle after the children are terminal. `blocked`, `delivery_uncertain`,
/// `failed`, and pane-closed terminals never reach this path: a blocked
/// supervisor must surface at once because nothing would wake it again.
fn hold_settled_task_for_children(task: &mut PromptTaskRecord, awaiting_children: u32) {
    task.lifecycle = PromptTaskLifecycle::Running;
    task.awaiting_children = Some(awaiting_children);
    task.terminal_status = None;
    task.terminal_cursor = None;
    task.terminal_at_ms = None;
    task.final_excerpt = None;
}

/// Returns true when this same event batch already proves the pane moved past
/// the task's terminal cursor, i.e. the pane may be serving a later task. A
/// later same-pane status change or pane closure is by itself enough to fail the
/// capture closed before any native read; a differing sequence does the same when
/// events carry one. A `pane_updated` snapshot of the same terminal status is the
/// companion of `pane.agent_status_changed` and does not prove a later task; it
/// only matters once the terminal event carries a sequence, because an
/// unsequenced terminal event never reaches the read at all.
fn batch_proves_pane_advanced(
    task: &PromptTaskRecord,
    events: &[Value],
    terminal_state_change_seq: Option<u64>,
) -> bool {
    let terminal_cursor = task.terminal_cursor.unwrap_or(0);
    events.iter().any(|event| {
        event_matches_task_pane(task, event)
            && event.get("cursor").and_then(Value::as_u64).unwrap_or(0) > terminal_cursor
            && event_proves_pane_advanced(task, event, terminal_state_change_seq)
    })
}

/// Only two observations are the same transition and may pass the fence:
/// the same status with a `state_change_seq` present on both sides and equal,
/// or a `pane_updated` snapshot of that same status with no sequence. Live
/// Herdr emits the snapshot immediately after `pane.agent_status_changed`.
/// A second `pane.agent_status_changed` with the same status and no sequence
/// is a later task that already finished.
fn event_proves_pane_advanced(
    task: &PromptTaskRecord,
    event: &Value,
    terminal_state_change_seq: Option<u64>,
) -> bool {
    if matches!(
        task_event_type(event).as_str(),
        "pane_closed" | "pane_exited"
    ) {
        return true;
    }
    let same_status = task_event_status(event) == task.terminal_status.as_deref();
    if !same_status {
        return true;
    }
    let event_seq = task_event_state_change_seq(event);
    if event_seq.is_some() && event_seq == terminal_state_change_seq {
        return false;
    }
    task_event_type(event) != "pane_updated" || event_seq.is_some()
}

fn task_event_state_change_seq(event: &Value) -> Option<u64> {
    event
        .get("state_change_seq")
        .and_then(Value::as_u64)
        .or_else(|| {
            event
                .get("pane")
                .and_then(|pane| pane.get("state_change_seq"))
                .and_then(Value::as_u64)
        })
}

/// Terminal lifecycles that can carry a captured outcome. Excludes
/// `delivery_uncertain` and pane closure/exiting, which have no readable
/// agent outcome.
fn final_excerpt_eligible(task: &PromptTaskRecord) -> bool {
    matches!(
        task.lifecycle,
        PromptTaskLifecycle::Completed | PromptTaskLifecycle::Blocked | PromptTaskLifecycle::Failed
    ) && !matches!(
        task.terminal_status.as_deref(),
        Some("pane_closed" | "pane_exited")
    )
}

/// Reads the child's recent unwrapped output through the same native Herdr
/// socket path the prompt dispatch uses, then bounds it for durable storage.
///
/// A pane may be reused between terminal-event observation and this read, so the
/// capture is fenced on a `state_change_seq` observed both before and after the
/// read. That sequence must come from the terminal event itself: Herdr 0.8.2 and
/// 0.9.5 emit no sequence, and a probe-only sequence cannot tell this task's
/// settle from a later same-status task on the reused pane, so such a transition
/// is never read and simply keeps `final_excerpt` unset.
fn read_final_excerpt(
    client: Option<&HerdrClient>,
    task: &PromptTaskRecord,
    expected_state_change_seq: Option<u64>,
) -> Option<String> {
    let client = client?;
    let target = task.pane_id.as_deref().unwrap_or(task.target.as_str());
    let capture_seq = final_excerpt_capture_seq(client, task, target, expected_state_change_seq)?;

    let response = client
        .call_with_timeout(
            "agent.read",
            json!({
                "target": target,
                "source": "recent_unwrapped",
                "lines": FINAL_EXCERPT_LINES,
                "strip_ansi": true,
            }),
            FINAL_EXCERPT_READ_TIMEOUT,
        )
        .ok()?;
    let read = response.get("read").unwrap_or(&response);
    let text = ["content", "text", "output"]
        .into_iter()
        .find_map(|key| read.get(key).and_then(Value::as_str))?;
    let excerpt = shape_final_excerpt(text);
    if excerpt.trim().is_empty() {
        return None;
    }

    // Reject output if the pane advanced during the read: the same pane, the
    // same terminal status and the same state sequence must all still hold.
    let state = final_excerpt_agent_state(client, target)?;
    if state.pane_id.as_deref() != Some(target)
        || state.state_change_seq != Some(capture_seq)
        || state.agent_status.as_deref() != task.terminal_status.as_deref()
    {
        return None;
    }
    // A pane narrower than the readable floor stores hard-wrapped viewport
    // fragments (a TUI agent wraps its own layout, so `recent_unwrapped` has no
    // soft-wrap to rejoin). Its logical lines still exist in the agent's own
    // session transcript, so read those instead of publishing wrapping noise
    // (#684). Unknown geometry fails closed: never publish rows that cannot be
    // qualified as the agent's own output.
    match final_excerpt_pane_columns(client, target) {
        Some(cols) if crate::pane_width::excerpt_is_readable(cols) => Some(excerpt),
        Some(_) => logical_excerpt_from_agent_session(
            client,
            target,
            capture_seq,
            task.terminal_status.as_deref(),
        ),
        None => None,
    }
}

/// Bytes of the agent transcript tail inspected for the last assistant message.
const FINAL_EXCERPT_SESSION_TAIL_BYTES: u64 = 256 * 1024;

/// Recovers the child's last assistant message from its own session file.
///
/// This is the logical-line path for panes too narrow to read: it returns the
/// text the agent produced, not the column-wrapped rows it painted. The
/// filesystem read is slower than a pane read, so the same pane-reuse fence is
/// applied twice: the `agent.get` that resolves the session path must already
/// sit on this task's terminal transition, and the pane must still report the
/// same identity, status and `state_change_seq` after the transcript is read.
/// Anything unreadable, unparsable or moved leaves the excerpt absent.
fn logical_excerpt_from_agent_session(
    client: &HerdrClient,
    target: &str,
    capture_seq: u64,
    terminal_status: Option<&str>,
) -> Option<String> {
    let response = client
        .call_with_timeout(
            "agent.get",
            json!({"target": target}),
            FINAL_EXCERPT_VERIFY_TIMEOUT,
        )
        .ok()?;
    if !pane_still_serves_capture(&response, target, capture_seq, terminal_status) {
        return None;
    }
    let agent = response.get("agent").unwrap_or(&response);
    let session = agent.get("agent_session")?;
    if session.get("kind").and_then(Value::as_str) != Some("path") {
        return None;
    }
    let path = session.get("value").and_then(Value::as_str)?;
    let text = read_session_tail_text(Path::new(path))?;
    let excerpt = shape_final_excerpt(&text);
    if excerpt.trim().is_empty() {
        return None;
    }
    // A pane reused while the transcript was read would make this excerpt
    // belong to a later task, so re-prove the fence after the read.
    let after = client
        .call_with_timeout(
            "agent.get",
            json!({"target": target}),
            FINAL_EXCERPT_VERIFY_TIMEOUT,
        )
        .ok()?;
    pane_still_serves_capture(&after, target, capture_seq, terminal_status).then_some(excerpt)
}

/// True when the pane still reports this task's pane id, terminal status and
/// captured state sequence.
fn pane_still_serves_capture(
    response: &Value,
    target: &str,
    capture_seq: u64,
    terminal_status: Option<&str>,
) -> bool {
    agent_state_from_value(response).is_some_and(|state| {
        state.pane_id.as_deref() == Some(target)
            && state.state_change_seq == Some(capture_seq)
            && state.agent_status.as_deref() == terminal_status
    })
}

/// Last assistant message text from the tail of a JSONL agent transcript.
fn read_session_tail_text(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(FINAL_EXCERPT_SESSION_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = String::new();
    file.take(FINAL_EXCERPT_SESSION_TAIL_BYTES)
        .read_to_string(&mut tail)
        .ok()?;
    tail.lines()
        .rev()
        .find_map(assistant_text_from_session_line)
}

fn assistant_text_from_session_line(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let message = value.get("message").unwrap_or(&value);
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    match message.get("content") {
        Some(Value::String(text)) => (!text.trim().is_empty()).then(|| text.clone()),
        Some(Value::Array(blocks)) => {
            let text: Vec<&str> = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        _ => None,
    }
}

/// Column width of the pane the excerpt was read from. A missing or failed
/// `pane.layout` read is not treated as readable: the caller fails closed
/// because rows that cannot be qualified may be wrapping noise.
fn final_excerpt_pane_columns(client: &HerdrClient, target: &str) -> Option<u16> {
    let response = client
        .call_with_timeout(
            "pane.layout",
            json!({"pane_id": target}),
            FINAL_EXCERPT_VERIFY_TIMEOUT,
        )
        .ok()?;
    crate::pane_width::pane_columns_from_layout_response(&response, target)
}

/// Returns the pane state sequence this capture is fenced on, after proving the
/// pane still reports this task's terminal status.
///
/// The fence fails closed without the producer's own terminal identity. Herdr
/// 0.8.2 and 0.9.5 emit `pane.agent_status_changed` without `state_change_seq`,
/// so a deferred `agent.get` only reports "whatever the pane serves now": a
/// later task that settled to the same status on the same pane is
/// indistinguishable from this task's own settle, and the pre/post probes agree
/// on the impostor. Capturing there would store the wrong child's output, so an
/// event without a sequence omits the excerpt until the producer stamps one.
fn final_excerpt_capture_seq(
    client: &HerdrClient,
    task: &PromptTaskRecord,
    target: &str,
    expected_state_change_seq: Option<u64>,
) -> Option<u64> {
    let expected_seq = expected_state_change_seq?;
    let pre = final_excerpt_agent_state(client, target)?;
    if pre.pane_id.as_deref() != Some(target)
        || pre.agent_status.as_deref() != task.terminal_status.as_deref()
    {
        return None;
    }
    let pre_seq = pre.state_change_seq?;
    if expected_seq != pre_seq {
        return None;
    }
    // Terminal task state is monotonic: a sequence that did not move past the
    // dispatch baseline cannot belong to this task's terminal transition.
    if task
        .baseline_seq
        .is_some_and(|base_seq| pre_seq <= base_seq)
    {
        return None;
    }
    Some(pre_seq)
}

fn final_excerpt_agent_state(client: &HerdrClient, target: &str) -> Option<AgentState> {
    let response = client
        .call_with_timeout(
            "agent.get",
            json!({"target": target}),
            FINAL_EXCERPT_VERIFY_TIMEOUT,
        )
        .ok()?;
    agent_state_from_value(&response)
}

/// Strips ANSI/control noise, drops trailing blank lines, and keeps at most
/// `FINAL_EXCERPT_MAX_BYTES` bytes from the end on a UTF-8 boundary; a
/// leading `…` marks truncation inside the same byte budget.
fn shape_final_excerpt(text: &str) -> String {
    let stripped = strip_ansi_and_control(text);
    let trimmed = trim_trailing_blank_lines(&stripped);
    cap_bytes_from_end(trimmed, FINAL_EXCERPT_MAX_BYTES)
}

/// Removes ANSI escape sequences and control characters while keeping
/// newlines and tabs so the excerpt stays line-structured.
fn strip_ansi_and_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(current) = chars.next() {
        match current {
            '\u{1b}' => skip_ansi_sequence(&mut chars),
            '\n' | '\t' => out.push(current),
            current if current.is_control() => {}
            current => out.push(current),
        }
    }
    out
}

fn skip_ansi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek() {
        // CSI: parameters run until the alphabetic final byte.
        Some('[') => {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        // OSC: runs until BEL or the ESC \\ string terminator.
        Some(']') => {
            chars.next();
            for next in chars.by_ref() {
                if next == '\u{7}' {
                    break;
                }
                if next == '\u{1b}' {
                    let _ = chars.next();
                    break;
                }
            }
        }
        // Two-byte escapes: consume the single terminator byte.
        _ => {
            let _ = chars.next();
        }
    }
}

/// Drops trailing blank lines (the line plus its newline) from the end.
fn trim_trailing_blank_lines(text: &str) -> &str {
    let mut end = text.len();
    for line in text.lines().rev() {
        if line.trim().is_empty() {
            end = end.saturating_sub(line.len() + 1);
        } else {
            break;
        }
    }
    &text[..end.min(text.len())]
}

/// Keeps at most `max_bytes` bytes measured from the end; the `…` truncation
/// marker fits inside the same budget so the result never exceeds it.
fn cap_bytes_from_end(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker_len = '…'.len_utf8().min(max_bytes);
    let mut start = text.len() + marker_len - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
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
            Some(i64::try_from(expires_at).unwrap_or(i64::MAX)),
        )
        .map_err(|error| {
            json!({
                "ok": false,
                "code": "idempotency_store_unavailable",
                "message": format!("durable idempotency reservation failed: {error}"),
                "retryable": false,
                "hint": "The prompt was not submitted because durable state could not be recorded. A new submission requires a healthy local state store.",
            })
        })?;

    match reservation {
        OperationReservation::Reserved => {
            // Reusing a key after its short replay reservation expires must
            // never submit new work while the durable task still owns it.
            let digest = stable_digest(&[b"herdr_prompt/task_key", key.as_bytes()]);
            let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
            let existing = store
                .task_operation_ids_by_key_digest(PROMPT_TASK_OPERATION_KIND, &digest, now_i64)
                .and_then(|mut ids| {
                    if ids.is_empty() {
                        // Before this release, task rows were not indexed by key.
                        // Their deterministic task_id still guards exact retries.
                        let old_id = dispatch_id(Some(key), fingerprint);
                        if let Some(record) = store.operation_ledger(&old_id)?
                            && record.kind == PROMPT_TASK_OPERATION_KIND
                            && record.expires_at.is_none_or(|expiry| expiry > now_i64)
                        {
                            ids.push(old_id);
                        }
                    }
                    Ok(ids)
                });
            if matches!(&existing, Ok(ids) if ids.is_empty()) {
                return Ok(BeginPrompt::Reserved(Some(op_id)));
            }
            store
                .release_operation_reservation(
                    PROMPT_OPERATION_KIND,
                    &key_hash,
                    fingerprint,
                    &op_id,
                )
                .map_err(|error| {
                    json!({"ok": false, "code": "idempotency_store_unavailable", "message": error, "retryable": false})
                })?;
            match existing {
                Ok(_) => Err(json!({
                    "ok": false,
                    "code": "agent_task_idempotency_key_in_use",
                    "retryable": false,
                    "hint": "A durable task still owns this key. Query agent.task.status with idempotency_key and target, or use a new key for new work."
                })),
                Err(error) => Err(json!({
                    "ok": false,
                    "code": "idempotency_store_unavailable",
                    "message": error,
                    "retryable": false,
                })),
            }
        }
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
                    "hint": "A durable submission reservation exists without a settled outcome. Live agent state is required before another submission; a runtime restart does not clear the reservation.",
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
            "hint": "The prompt may already have been submitted. Repeating it can duplicate work; live agent state distinguishes the outcome.",
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
            "hint": "The prompt may already have been submitted. Repeating it can duplicate work; live agent state distinguishes the outcome.",
        })
    })?;
    store
        .complete_operation(
            PROMPT_OPERATION_KIND,
            &key_hash,
            fingerprint,
            &result_json,
            i64::try_from(now).unwrap_or(i64::MAX),
            Some(i64::try_from(expires_at).unwrap_or(i64::MAX)),
        )
        .map_err(|error| {
            json!({
                "ok": false,
                "code": "idempotency_completion_persist_failed",
                "message": format!("durable idempotency completion failed: {error}"),
                "retryable": false,
                "hint": "The prompt may already have been submitted. Repeating it can duplicate work; live agent state distinguishes the outcome.",
            })
        })
}

fn annotate_idempotency_persistence(mut result: Value, persistence_error: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("idempotency_persistence".to_owned(), persistence_error);
        object.insert("idempotency_persisted".to_owned(), json!(false));
        object.insert(
            "idempotency_hint".to_owned(),
            json!("The prompt outcome may already be committed while durable replay metadata is incomplete; live agent state determines whether another submission is safe."),
        );
    }
    result
}

pub const AGENT_TASK_DISPATCH_METHOD: &str = "herdr_mcp.agent.task.dispatch";
pub const AGENT_TASK_REPORT_METHOD: &str = "herdr_mcp.agent.task.report";
pub const AGENT_TASK_STATUS_METHOD: &str = "herdr_mcp.agent.task.status";
pub const AGENT_TASK_INBOX_METHOD: &str = "herdr_mcp.agent.task.inbox";
pub const AGENT_TASK_ACK_METHOD: &str = "herdr_mcp.agent.task.ack";

pub fn task_call_with_parent_session(
    registry: &PromptRegistry,
    method: &str,
    params: &Value,
    caller_parent_session_ref: Option<&str>,
) -> Value {
    match method {
        AGENT_TASK_REPORT_METHOD => {
            if caller_parent_session_ref.is_some() {
                return json!({"ok": false, "code": "worker_report_requires_trusted_local"});
            }
            registry.task_report(params)
        }
        AGENT_TASK_STATUS_METHOD => {
            if let Some(key_value) = params.get("idempotency_key") {
                let Some(key) = key_value.as_str() else {
                    return invalid("idempotency_key must be a string");
                };
                let Some(target) = params.get("target").and_then(Value::as_str) else {
                    return invalid("target is required for idempotency lookup");
                };
                if key.is_empty() || key.len() > 256 || target.is_empty() || target.len() > 256 {
                    return invalid("invalid idempotency_key or target length");
                }
                if params.get("task_id").is_some() {
                    return invalid("choose task_id or idempotency_key, not both");
                }
                return registry.task_status_by_idempotency(key, target, caller_parent_session_ref);
            }
            let Some(task_id) = params.get("task_id").and_then(Value::as_str) else {
                return invalid("task_id is required");
            };
            if task_id.is_empty() || task_id.len() > 128 {
                return invalid("task_id must be a non-empty string up to 128 bytes");
            }
            if let Some(parent_session_ref) = caller_parent_session_ref {
                registry.task_status_for_parent_session(task_id, parent_session_ref)
            } else {
                registry.task_status(task_id)
            }
        }
        AGENT_TASK_INBOX_METHOD => {
            let workspace_id = params.get("workspace_id").and_then(Value::as_str);
            let parent_target = params.get("parent_target").and_then(Value::as_str);
            let parent_session_ref = caller_parent_session_ref
                .or_else(|| params.get("parent_session_ref").and_then(Value::as_str));
            let include_acknowledged = params
                .get("include_acknowledged")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let advisory = params
                .get("advisory")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 512) as usize;
            let wait_ms = match parse_inbox_wait_ms(params) {
                Ok(wait_ms) => wait_ms,
                Err(error) => return error,
            };
            let view = match parse_inbox_view(params) {
                Ok(view) => view,
                Err(error) => return error,
            };
            let query = InboxQuery {
                workspace_id,
                parent_target,
                parent_session_ref,
                include_acknowledged,
                advisory,
                limit,
            };
            let inbox = match wait_ms {
                Some(wait_ms) => registry.task_inbox_waiting(&query, wait_ms),
                None => registry.task_inbox(
                    workspace_id,
                    parent_target,
                    parent_session_ref,
                    include_acknowledged,
                    advisory,
                    limit,
                ),
            };
            apply_inbox_view(inbox, view)
        }
        AGENT_TASK_ACK_METHOD => {
            if let Some(task_ids) = params.get("task_ids") {
                if params.get("task_id").is_some() {
                    return invalid("choose task_id or task_ids, not both");
                }
                let task_ids = match parse_task_ids(task_ids) {
                    Ok(task_ids) => task_ids,
                    Err(error) => return error,
                };
                let borrowed = task_ids.iter().map(String::as_str).collect::<Vec<_>>();
                return registry.task_ack_many(&borrowed, caller_parent_session_ref);
            }
            let Some(task_id) = params.get("task_id").and_then(Value::as_str) else {
                return invalid("task_id is required");
            };
            if task_id.is_empty() || task_id.len() > 128 {
                return invalid("task_id must be a non-empty string up to 128 bytes");
            }
            if let Some(parent_session_ref) = caller_parent_session_ref {
                registry.task_ack_for_parent_session(task_id, parent_session_ref)
            } else {
                registry.task_ack(task_id)
            }
        }
        _ => json!({
            "ok": false,
            "code": "unknown_agent_task_method",
            "method": method,
        }),
    }
}

pub fn run(client: &HerdrClient, registry: &PromptRegistry, args: &Value) -> Value {
    run_with_parent_session(client, registry, args, None)
}

/// Rejections for a dispatch that would make the delegation chain unsafe. Both
/// guards run before the idempotency reservation and before the native prompt,
/// so a rejected dispatch writes no ledger row and prompts no agent.
fn delegation_rejection(code: &str, message: &str) -> Value {
    json!({
        "ok": false,
        "code": code,
        "message": message,
        "retryable": false,
    })
}

/// Refuses self-parenting and a third delegation level below the top-level
/// parent.
///
/// The Web planner may dispatch a supervisor, and that supervisor may dispatch
/// workers (`parent_target` naming the supervisor). A worker dispatching its own
/// sub-worker is refused: `parent_target` then names an active task that is
/// itself the child of another active task.
fn delegation_guard(
    client: &HerdrClient,
    registry: &PromptRegistry,
    target: &str,
    target_state: Option<&AgentState>,
    parent_target: &str,
) -> Option<Value> {
    let target_pane = target_state.and_then(|state| state.pane_id.as_deref());
    let parent_pane = agent_state_of(client, parent_target).and_then(|state| state.pane_id);
    let same_target = parent_target == target
        || parent_pane
            .as_deref()
            .zip(target_pane)
            .is_some_and(|(parent_pane, target_pane)| parent_pane == target_pane);
    if same_target {
        return Some(delegation_rejection(
            "parent_target_is_target",
            "parent_target resolves to the same agent or pane as target; a task cannot be its own parent",
        ));
    }
    match delegation_depth_exceeded(registry, parent_target, parent_pane.as_deref()) {
        Ok(false) => None,
        Ok(true) => Some(delegation_rejection(
            "delegation_depth_exceeded",
            "parent_target is already the child of another active task; at most two delegation levels below the top-level parent are allowed",
        )),
        // The chain lives in the durable task ledger: without it the depth cap
        // cannot be proven, so fail closed instead of allowing the dispatch.
        Err(error) => Some(json!({
            "ok": false,
            "code": "agent_task_store_unavailable",
            "message": error,
            "retryable": true,
        })),
    }
}

/// True when `parent_target` names an active task whose own `parent_target`
/// names another active task. Only the durable ledger is consulted: every
/// recorded `parent_target` is stored exactly as its dispatcher wrote it, so the
/// aliases stay comparable without a live probe.
fn delegation_depth_exceeded(
    registry: &PromptRegistry,
    parent_target: &str,
    parent_pane: Option<&str>,
) -> Result<bool, String> {
    let tasks = registry.list_tasks(512)?;
    let active = |task: &PromptTaskRecord| !task.is_terminal();
    let Some(parent_task) = tasks
        .iter()
        .find(|task| active(task) && task_matches_target(task, parent_target, parent_pane))
    else {
        return Ok(false);
    };
    let Some(grandparent_target) = parent_task.parent_target.as_deref() else {
        return Ok(false);
    };
    Ok(tasks
        .iter()
        .any(|task| active(task) && task_matches_target(task, grandparent_target, None)))
}

pub fn run_with_parent_session(
    client: &HerdrClient,
    registry: &PromptRegistry,
    args: &Value,
    parent_session_ref: Option<&str>,
) -> Value {
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
    let parent_target = match optional_str(args, "parent_target") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let stall_after_ms = match args.get("stall_after_ms") {
        None => None,
        Some(value) => match value.as_u64() {
            Some(value) if (60_000..=120 * 60_000).contains(&value) => Some(value),
            _ => return invalid("stall_after_ms must be an integer in 60000..=7200000"),
        },
    };
    let inject_task_identity = match args.get("inject_task_identity") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return invalid("inject_task_identity must be a boolean"),
    };
    let wait = match parse_wait(args.get("wait")) {
        Ok(value) => value,
        Err(error) => return error,
    };
    // Observation preferences such as wait are not part of mutation identity.
    // The same target/text intent must keep the same task under one idempotency key.
    let fingerprint = request_fingerprint(target, text);
    let dispatch_id = dispatch_id(idempotency_key, &fingerprint);
    let task_id = dispatch_id.as_str();
    let mut operation_id = None;

    // A parented dispatch probes the caller's own agent once for the delegation
    // guards and reuses that observation as the pre-submit baseline below.
    let guard_observation = parent_target.and_then(|_| agent_state_of(client, target));
    if let Some(parent_target) = parent_target
        && let Some(mut rejection) = delegation_guard(
            client,
            registry,
            target,
            guard_observation.as_ref(),
            parent_target,
        )
    {
        insert_dispatch_id(&mut rejection, task_id);
        insert_task_id(&mut rejection, task_id);
        return rejection;
    }

    if let Some(key) = idempotency_key {
        match registry.begin(key, &fingerprint) {
            Ok(BeginPrompt::Reserved(op_id)) => operation_id = op_id,
            Ok(BeginPrompt::Replay(mut result)) => {
                insert_dispatch_id(&mut result, task_id);
                insert_task_id(&mut result, task_id);
                attach_task_observation(registry, &mut result, task_id, None);
                if let Some(wait) = wait.as_ref() {
                    apply_task_wait(registry, &mut result, task_id, wait);
                }
                return result;
            }
            Err(mut error) => {
                insert_dispatch_id(&mut error, task_id);
                insert_task_id(&mut error, task_id);
                return error;
            }
        }
    }

    let start_cursor = registry.current_task_cursor();
    let before = guard_observation.or_else(|| agent_state_of(client, target));
    let pane_id = before
        .as_ref()
        .and_then(|state| state.pane_id.as_deref())
        .or_else(|| registry.pane_worktree(target).map(|_| target));
    let handoff_target = pane_id.and_then(|pane_id| {
        registry
            .pane_worktree(pane_id)
            .map(|worktree| (pane_id.to_owned(), worktree))
    });
    let injection = handoff_target
        .as_ref()
        .map(|(pane_id, worktree)| registry.inject_handoff_once(pane_id, worktree, text));
    let mut dispatch_text = injection
        .as_ref()
        .map_or_else(|| text.to_owned(), |injection| injection.text.clone());
    if inject_task_identity {
        // Runtime-generated ids are bounded and contain no caller/session data.
        // Keep this after fingerprinting and the replay return above.
        dispatch_text.push_str(&format!(
            "\n[herdr-task task_id={task_id} dispatch_id={dispatch_id}]\nReport completion exactly once: herdr-mcp agent report --task-id {task_id} --dispatch-id {dispatch_id} --kind worker_done. For long tasks, report --kind heartbeat periodically."
        ));
    }
    // Native Herdr 0.9.x wait is lifecycle/status based and cannot identify the
    // prompt turn. Submit exactly once without native wait, then wait on the
    // Runtime-owned task ledger below.
    let params = json!({
        "target": target,
        "text": dispatch_text,
    });

    let mut result = match client.call_with_timeout(
        "agent.prompt",
        params,
        Duration::from_millis(DEFAULT_CALL_TIMEOUT_MS),
    ) {
        Ok(response) => prompt_success(
            client,
            target,
            before.as_ref(),
            false,
            response,
            idempotency_key.is_none(),
            task_id,
        ),
        Err(error) => prompt_failure(client, target, before.as_ref(), false, error, task_id),
    };
    insert_task_id(&mut result, task_id);

    // A dispatch that provably never reached the agent releases the mark it
    // created so the next first dispatch re-attaches the handoff. Success and
    // uncertain delivery keep the mark: the handoff may already be delivered.
    if let (Some((pane_id, _)), Some(injection)) = (&handoff_target, &injection)
        && injection.marked_now
        && dispatch_definitely_not_delivered(&result)
    {
        registry.unmark_handoff_injected(pane_id);
    }

    let submitted = result.get("submitted").and_then(Value::as_bool);
    let after = agent_state_from_result(&result);
    match registry.register_task(RegisterTaskInput {
        task_id,
        target,
        fingerprint: &fingerprint,
        idempotency_key,
        before: before.as_ref(),
        after: after.as_ref(),
        submitted,
        start_cursor,
        parent_target,
        parent_session_ref,
        stall_after_ms,
    }) {
        Ok(Some(task)) => {
            if let Some(object) = result.as_object_mut() {
                object.insert("task_tracking".to_owned(), json!(true));
                object.insert("turn_id".to_owned(), json!(task.turn_id));
                object.insert("task".to_owned(), task.public_json_at(registry.now_ms()));
            }
        }
        Ok(None) => {}
        Err(error) => {
            if let Some(object) = result.as_object_mut() {
                object.insert("task_tracking".to_owned(), json!(false));
                object.insert(
                    "task_persistence".to_owned(),
                    json!({
                        "ok": false,
                        "code": "agent_task_store_unavailable",
                        "message": error,
                    }),
                );
                object.insert(
                    "task_hint".to_owned(),
                    json!("the prompt may already be running, but durable task tracking is unavailable; do not blindly re-dispatch"),
                );
            }
        }
    }

    attach_task_observation(registry, &mut result, task_id, None);

    if let Some(op_id) = operation_id
        && let Some(object) = result.as_object_mut()
    {
        object.insert("op_id".to_owned(), json!(op_id));
    }

    // Persist only the submission/task identity outcome. A caller's optional
    // wait is an observation request and must not become the replay payload.
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

    if let Some(wait) = wait.as_ref() {
        apply_task_wait(registry, &mut result, task_id, wait);
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
    dispatch_id: &str,
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
    insert_dispatch(
        &mut result,
        dispatch_id,
        dispatch_correlation(
            before,
            after.as_ref(),
            waited,
            waited,
            Some(status != "agent_blocked"),
        ),
    );
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
    dispatch_id: &str,
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
        insert_dispatch(
            &mut result,
            dispatch_id,
            dispatch_correlation(before, after.as_ref(), true, false, submitted.as_bool()),
        );
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
        insert_dispatch(
            &mut result,
            dispatch_id,
            dispatch_correlation(before, after.as_ref(), waited, false, submitted.as_bool()),
        );
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
    insert_dispatch(
        &mut result,
        dispatch_id,
        dispatch_correlation(
            before,
            after.as_ref(),
            waited,
            false,
            if resolve_failure || retryable {
                Some(false)
            } else {
                None
            },
        ),
    );
    if resolve_failure {
        result.insert("submitted".to_owned(), json!(false));
        result.insert("delivery_state".to_owned(), json!("not_delivered"));
        result.insert("requires_human".to_owned(), json!(false));
        result.insert(
            "recovery".to_owned(),
            json!({
                "action": "start_agent_then_reprompt",
                "inspect": "herdr_inspect",
                "split_method": "pane.split",
                "start_method": "agent.start",
                "reuse_existing_pane_only_when": "verified_task_owned_free_shell",
                "reprompt": "same_idempotency_key",
                "reclaim": "planner_created_pane_only"
            }),
        );
        result.insert(
            "hint".to_owned(),
            json!(
                "agent target did not resolve and nothing was submitted. This is a pre-dispatch condition, not a human boundary. Re-read herdr_inspect, select a compatible startable Agent, and use a verified task-owned free shell pane or pane.split a task-owned pane before agent.start. Re-prompt with the same idempotency_key after the Agent is ready; never infer pane ownership from agent:null."
            ),
        );
    } else if !retryable {
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

fn dispatch_id(idempotency_key: Option<&str>, fingerprint: &str) -> String {
    let digest = if let Some(key) = idempotency_key {
        stable_digest(&[
            b"herdr_prompt/dispatch/idempotent",
            key.as_bytes(),
            fingerprint.as_bytes(),
        ])
    } else {
        let now = now_ms().to_le_bytes();
        let nonce = NEXT_DISPATCH_NONCE
            .fetch_add(1, Ordering::Relaxed)
            .to_le_bytes();
        stable_digest(&[
            b"herdr_prompt/dispatch/ephemeral",
            fingerprint.as_bytes(),
            &now,
            &nonce,
        ])
    };
    format!("dispatch:prompt:{}", &digest[..32])
}

fn insert_dispatch_id(result: &mut Value, dispatch_id: &str) {
    if let Some(object) = result.as_object_mut() {
        object
            .entry("dispatch_id".to_owned())
            .or_insert_with(|| json!(dispatch_id));
    }
}

fn insert_task_id(result: &mut Value, task_id: &str) {
    if let Some(object) = result.as_object_mut() {
        object
            .entry("task_id".to_owned())
            .or_insert_with(|| json!(task_id));
    }
}

fn attach_task_observation(
    registry: &PromptRegistry,
    result: &mut Value,
    task_id: &str,
    wait: Option<&Value>,
) {
    if wait.is_some() {
        return;
    }
    let status = registry.task_status(task_id);
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return;
    }
    if let Some(object) = result.as_object_mut()
        && let Some(task) = status.get("task")
    {
        if let Some(turn_id) = task.get("turn_id") {
            object.insert("turn_id".to_owned(), turn_id.clone());
        }
        object.insert("task".to_owned(), task.clone());
    }
}

fn apply_task_wait(registry: &PromptRegistry, result: &mut Value, task_id: &str, wait: &Value) {
    let task_wait = registry.wait_task(task_id, wait);
    let completed = task_wait
        .get("completed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(object) = result.as_object_mut() {
        if let Some(task) = task_wait.get("task") {
            object.insert("task".to_owned(), task.clone());
        }
        object.insert("wait".to_owned(), task_wait);
        if !completed && object.get("ok").and_then(Value::as_bool) == Some(true) {
            if object["wait"]["outcome"] == "checkpoint" {
                object.insert("hint".to_owned(), json!("Wait checkpoint: the task is still live; re-check agent.task.status or agent.task.inbox with the same task_id. Do not redispatch."));
                return;
            }
            object.insert("ok".to_owned(), json!(false));
            object.insert("failure".to_owned(), object["wait"]["reason"].clone());
            object.insert(
                "failure_phase".to_owned(),
                json!("post_submission_task_wait"),
            );
            object.insert("retryable".to_owned(), json!(false));
            object.insert(
                "hint".to_owned(),
                json!("The prompt may already be submitted; re-check task status/inbox before retrying."),
            );
        }
    }
}

fn agent_state_from_result(result: &Value) -> Option<AgentState> {
    let after = result.get("after")?;
    Some(AgentState {
        pane_id: result
            .get("resolved_pane")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_status: after
            .get("agent_status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        state_change_seq: after.get("state_change_seq").and_then(Value::as_u64),
    })
}

fn insert_dispatch(result: &mut Map<String, Value>, dispatch_id: &str, correlation: Value) {
    result.insert("dispatch_id".to_owned(), json!(dispatch_id));
    result.insert("dispatch_correlation".to_owned(), correlation);
}

fn dispatch_correlation(
    before: Option<&AgentState>,
    after: Option<&AgentState>,
    waited: bool,
    wait_completed: bool,
    submitted: Option<bool>,
) -> Value {
    let before_status = before.and_then(|state| state.agent_status.as_deref());
    let after_status = after.and_then(|state| state.agent_status.as_deref());
    let seq_changed = before.zip(after).is_some_and(|(before, after)| {
        before.state_change_seq.is_some()
            && after.state_change_seq.is_some()
            && before.state_change_seq != after.state_change_seq
    });
    let active_status_observed =
        matches!(after_status, Some("working" | "blocked")) && before_status != after_status;
    let note = "Herdr 0.9.1 exposes lifecycle state, not a native prompt turn id; this metadata uses a post-submit activity gate and never claims exact-turn identity.";

    if submitted == Some(false) {
        return json!({
            "mode": "not_submitted",
            "activity_observed": false,
            "state": "not_submitted",
            "terminal": false,
            "attention_required": before_status == Some("blocked") || after_status == Some("blocked"),
            "exact_turn": false,
            "note": note,
        });
    }

    if before_status == Some("working") {
        return json!({
            "mode": "unverified_preexisting_work",
            "activity_observed": false,
            "state": "unverified",
            "terminal": false,
            "attention_required": false,
            "exact_turn": false,
            "note": note,
        });
    }

    if before_status == Some("blocked") {
        return json!({
            "mode": "unverified_preexisting_blocked",
            "activity_observed": false,
            "state": "unverified",
            "terminal": false,
            "attention_required": true,
            "exact_turn": false,
            "note": note,
        });
    }

    let started_settled = matches!(before_status, Some("idle" | "done"));
    if !started_settled {
        return json!({
            "mode": "unverified_missing_settled_baseline",
            "activity_observed": false,
            "state": "unverified",
            "terminal": false,
            "attention_required": after_status == Some("blocked"),
            "exact_turn": false,
            "note": note,
        });
    }

    let activity_observed = seq_changed || active_status_observed || (waited && wait_completed);
    if !activity_observed {
        return json!({
            "mode": "awaiting_activity",
            "activity_observed": false,
            "state": "awaiting_activity",
            "terminal": false,
            "attention_required": false,
            "exact_turn": false,
            "note": note,
        });
    }

    let (state, terminal, attention_required) = match after_status {
        Some("working") => ("working", false, false),
        Some("blocked") => ("blocked_attention", true, true),
        Some("idle" | "done") => ("settled", true, false),
        _ => ("activity_observed", false, false),
    };
    json!({
        "mode": "activity_gated",
        "activity_observed": true,
        "state": state,
        "terminal": terminal,
        "attention_required": attention_required,
        "exact_turn": false,
        "note": note,
    })
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

fn request_fingerprint(target: &str, text: &str) -> String {
    stable_digest(&[b"herdr_prompt/request", target.as_bytes(), text.as_bytes()])
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

/// Worktree for a pane: the herdr-managed worktree checkout of its workspace
/// when present, otherwise the deepest managed Git project root containing the
/// pane (plain workspaces opened on a repo carry no `worktree` metadata). This
/// matches the worktree the tool_call capture and tailers key work_state by.
fn pane_worktree_from_snapshot(
    snapshot: &Value,
    pane_id: &str,
    workspace_id: Option<String>,
) -> Option<String> {
    let from_metadata = workspace_id.and_then(|workspace_id| {
        snapshot
            .get("workspaces")
            .and_then(Value::as_array)
            .and_then(|workspaces| {
                workspaces.iter().find(|workspace| {
                    workspace
                        .get("workspace_id")
                        .and_then(Value::as_str)
                        .or_else(|| workspace.get("id").and_then(Value::as_str))
                        == Some(&workspace_id)
                })
            })
            .and_then(|workspace| workspace.get("worktree"))
            .and_then(Value::as_object)
            .and_then(|worktree| {
                worktree
                    .get("checkout_path")
                    .and_then(Value::as_str)
                    .or_else(|| worktree.get("path").and_then(Value::as_str))
            })
            .map(str::to_owned)
    });
    from_metadata.or_else(|| {
        crate::projects::derive_routing(snapshot)
            .projects
            .values()
            .filter(|project| project.managed && project.pane_ids.iter().any(|p| p == pane_id))
            .max_by_key(|project| project.root.components().count())
            .map(|project| project.root.to_string_lossy().into_owned())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

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
    fn dispatch_correlation_requires_new_activity_and_rejects_preexisting_state() {
        let idle = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("idle".to_owned()),
            state_change_seq: Some(10),
        };
        let unchanged = dispatch_correlation(Some(&idle), Some(&idle), false, false, Some(true));
        assert_eq!(unchanged["mode"], "awaiting_activity");
        assert_eq!(unchanged["terminal"], false);

        let stale_done = AgentState {
            agent_status: Some("done".to_owned()),
            ..idle.clone()
        };
        let stale = dispatch_correlation(Some(&idle), Some(&stale_done), false, false, Some(true));
        assert_eq!(stale["mode"], "awaiting_activity");
        assert_eq!(stale["terminal"], false);

        let working = AgentState {
            agent_status: Some("working".to_owned()),
            state_change_seq: Some(11),
            ..idle.clone()
        };
        let observed = dispatch_correlation(Some(&idle), Some(&working), false, false, Some(true));
        assert_eq!(observed["state"], "working");
        assert_eq!(observed["activity_observed"], true);

        let blocked = AgentState {
            agent_status: Some("blocked".to_owned()),
            state_change_seq: Some(12),
            ..idle.clone()
        };
        let attention = dispatch_correlation(Some(&idle), Some(&blocked), false, false, Some(true));
        assert_eq!(attention["state"], "blocked_attention");
        assert_eq!(attention["terminal"], true);

        let stale_blocked =
            dispatch_correlation(Some(&blocked), Some(&blocked), false, false, Some(true));
        assert_eq!(stale_blocked["mode"], "unverified_preexisting_blocked");
        assert_eq!(stale_blocked["terminal"], false);

        let waited = dispatch_correlation(Some(&idle), Some(&stale_done), true, true, Some(true));
        assert_eq!(waited["state"], "settled");
        assert_eq!(waited["terminal"], true);

        let preexisting_work =
            dispatch_correlation(Some(&working), Some(&stale_done), true, true, Some(true));
        assert_eq!(preexisting_work["mode"], "unverified_preexisting_work");
        assert_eq!(preexisting_work["terminal"], false);
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
    fn first_pane_dispatch_injects_bounded_handoff_once() {
        use crate::state_store::work_journal::WorkState;

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        store
            .lock()
            .unwrap()
            .put_work_state(&WorkState {
                project_ref: "project-a".to_owned(),
                worktree: "/repo".to_owned(),
                handoff_md: "important context".to_owned(),
                through_event_id: 4,
                log_cursors_json: "{}".to_owned(),
                updated_at: 5,
            })
            .unwrap();
        let registry = PromptRegistry::with_store(store);
        let first = registry.inject_handoff_once("w1:p1", "/repo", "do next");
        assert!(first.marked_now);
        assert!(
            first
                .text
                .starts_with("## Handoff context\n\nimportant context")
        );
        assert!(first.text.ends_with("do next"));
        let second = registry.inject_handoff_once("w1:p1", "/repo", "do another");
        assert!(!second.marked_now);
        assert_eq!(second.text, "do another");
        assert!(
            registry
                .inject_handoff_once("w1:p2", "/repo", "other pane")
                .text
                .chars()
                .count()
                < 16_100
        );
    }

    fn scripted_dispatch_server(
        listener: UnixListener,
        responses: Vec<Value>,
        prompt_texts: Arc<Mutex<Vec<String>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for mut response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                if request["method"] == "agent.prompt" {
                    prompt_texts
                        .lock()
                        .unwrap()
                        .push(request["params"]["text"].as_str().unwrap().to_owned());
                }
                response["id"] = request["id"].clone();
                writeln!(stream, "{response}").unwrap();
            }
        })
    }

    /// user receives explicit identity | Given opt-in or default dispatch | When replayed after changing the opt-in | Then only one prompt is submitted with the original identity and text
    #[test]
    fn dispatch_identity_injection_is_opt_in_and_replay_safe() {
        for inject in [false, true] {
            let socket = temp_socket();
            let listener = UnixListener::bind(&socket).unwrap();
            let texts = Arc::new(Mutex::new(Vec::new()));
            let server = scripted_dispatch_server(
                listener,
                vec![
                    json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}}}),
                    json!({"result": {"prompt": {"status": "submitted", "agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 2}}}}),
                ],
                texts.clone(),
            );
            let client = HerdrClient::new(&socket);
            let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
            let registry = PromptRegistry::with_store(store.clone());
            let mut args = json!({"target": "w1:p1", "text": "original text\n", "idempotency_key": "identity-test"});
            if inject {
                args["inject_task_identity"] = json!(true);
            }
            let first = run(&client, &registry, &args);
            assert_eq!(first["ok"], true, "{first}");
            server.join().unwrap();
            let task_id = first["task_id"].as_str().unwrap();
            let dispatch_id = first["dispatch_id"].as_str().unwrap();
            let expected = if inject {
                format!(
                    "original text\n\n[herdr-task task_id={task_id} dispatch_id={dispatch_id}]\nReport completion exactly once: herdr-mcp agent report --task-id {task_id} --dispatch-id {dispatch_id} --kind worker_done. For long tasks, report --kind heartbeat periodically."
                )
            } else {
                "original text\n".to_owned()
            };
            assert_eq!(*texts.lock().unwrap(), vec![expected]);
            assert_eq!(first["task"]["task_id"], task_id);
            assert_eq!(first["task"]["dispatch_id"], dispatch_id);
            // Reload the durable ledger and toggle the option: target/text
            // still denote the original submission, never a second mutation.
            let registry = PromptRegistry::with_store(store);
            args["inject_task_identity"] = json!(!inject);
            let replay = run(&client, &registry, &args);
            assert_eq!(replay["idempotent_replay"], true, "{replay}");
            assert_eq!(replay["task_id"], task_id);
            assert_eq!(replay["dispatch_id"], dispatch_id);
            assert_eq!(texts.lock().unwrap().len(), 1);
            let status = registry.task_status(task_id);
            assert_eq!(status["task"]["dispatch_id"], dispatch_id);
            let mut task = registry.load_task(task_id).unwrap().unwrap();
            // Exercise the same durable terminal event path as runtime reconcile.
            assert!(apply_task_event(
                &mut task,
                &json!({
                    "type": "pane.agent_status_changed", "pane_id": "w1:p1",
                    "agent_status": "idle", "cursor": 1,
                })
            ));
            registry.save_task(&task, None).unwrap();
            for view in [InboxView::Full, InboxView::Compact] {
                let inbox = apply_inbox_view(
                    registry.task_inbox(None, None, None, false, false, 10),
                    view,
                );
                assert_eq!(inbox["tasks"][0]["task_id"], task_id, "{inbox}");
                assert_eq!(inbox["tasks"][0]["dispatch_id"], dispatch_id);
            }
            fs::remove_file(socket).unwrap();
        }
    }

    fn handoff_registry() -> PromptRegistry {
        use crate::state_store::work_journal::WorkState;

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        store
            .lock()
            .unwrap()
            .put_work_state(&WorkState {
                project_ref: "project-a".to_owned(),
                worktree: "/repo".to_owned(),
                handoff_md: "important context".to_owned(),
                through_event_id: 4,
                log_cursors_json: "{}".to_owned(),
                updated_at: 5,
            })
            .unwrap();
        let cache = Arc::new(EventCache::from_snapshot_for_test(json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1"}],
            "agents": [],
            "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}],
        })));
        PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: Some(cache),
                client: None,
            }),
        }
    }

    #[test]
    fn clear_dispatch_failure_releases_handoff_mark_and_retry_injects() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let prompt_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let idle = json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}}});
        let not_delivered = json!({"error": {"code": "agent_not_found", "message": "agent target did not resolve"}});
        let submitted = json!({"result": {"prompt": {"status": "submitted", "agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 2}}}});
        let server = scripted_dispatch_server(
            listener,
            vec![
                idle.clone(),
                not_delivered,
                idle.clone(),
                idle.clone(),
                submitted.clone(),
                idle,
                submitted,
            ],
            Arc::clone(&prompt_texts),
        );
        let client = HerdrClient::new(&socket);
        let registry = handoff_registry();

        let failed = run(
            &client,
            &registry,
            &json!({"target": "pi", "text": "first attempt"}),
        );
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["delivery_state"], "not_delivered");

        // The clearly failed dispatch released its mark: the next first
        // dispatch attaches the handoff again instead of losing it forever.
        let retried = run(
            &client,
            &registry,
            &json!({"target": "pi", "text": "second attempt"}),
        );
        assert_eq!(retried["ok"], true);

        // After a successful dispatch the mark holds: no re-injection.
        let settled = run(
            &client,
            &registry,
            &json!({"target": "pi", "text": "third attempt"}),
        );
        assert_eq!(settled["ok"], true);
        server.join().unwrap();

        let texts = prompt_texts.lock().unwrap();
        assert_eq!(texts.len(), 3);
        assert!(texts[0].starts_with("## Handoff context\n\nimportant context"));
        assert!(texts[0].ends_with("first attempt"));
        assert!(texts[1].starts_with("## Handoff context\n\nimportant context"));
        assert!(texts[1].ends_with("second attempt"));
        assert_eq!(texts[2], "third attempt");
        fs::remove_file(socket).unwrap();
    }

    #[test]
    fn uncertain_dispatch_failure_keeps_handoff_mark() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let prompt_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let idle = json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}}});
        let lost_response = json!({"error": {"code": "socket_error", "message": "stream broke while reading the response"}});
        let server = scripted_dispatch_server(
            listener,
            vec![idle.clone(), lost_response, idle],
            Arc::clone(&prompt_texts),
        );
        let client = HerdrClient::new(&socket);
        let registry = handoff_registry();

        let failed = run(
            &client,
            &registry,
            &json!({"target": "pi", "text": "maybe delivered"}),
        );
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["delivery_uncertain"], true);
        server.join().unwrap();
        assert!(prompt_texts.lock().unwrap()[0].starts_with("## Handoff context"));
        // Delivery was uncertain: the pane keeps its mark so a retry does not
        // duplicate the handoff into the conversation.
        let again = registry.inject_handoff_once("w1:p1", "/repo", "second attempt");
        assert!(!again.marked_now);
        assert_eq!(again.text, "second attempt");
        fs::remove_file(socket).unwrap();
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
        assert_eq!(first["dispatch_correlation"]["state"], "working");
        let dispatch_id = first["dispatch_id"].clone();
        assert!(
            dispatch_id
                .as_str()
                .unwrap()
                .starts_with("dispatch:prompt:")
        );
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["dispatch_id"], dispatch_id);
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
        let dispatch_id = first["dispatch_id"].clone();
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
        assert_eq!(replay["dispatch_id"], dispatch_id);

        let waited_replay = run(
            &unreachable,
            &second_registry,
            &json!({
                "target": "pi",
                "text": "durable work",
                "idempotency_key": "prompt-persisted-1",
                "wait": {"until": ["working"], "timeout_ms": 1000}
            }),
        );
        assert_eq!(waited_replay["idempotent_replay"], true);
        assert_eq!(waited_replay["dispatch_id"], dispatch_id);
        assert_eq!(waited_replay["task_id"], dispatch_id);
        assert_eq!(waited_replay["wait"]["completed"], true);
        assert_eq!(waited_replay["wait"]["matched"], "running");

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

    #[test]
    fn task_lifecycle_is_activity_gated_durable_and_acknowledgeable() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let semantic_config_dir = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-semantic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &semantic_config_dir);
        }
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-ledger-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry::with_store(store);
        let before = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("working".to_owned()),
            state_change_seq: Some(10),
        };
        let after = before.clone();
        let task_id = "dispatch:prompt:task-lifecycle-test";
        let initial = registry
            .register_task(RegisterTaskInput {
                task_id,
                target: "child",
                fingerprint: "fingerprint",
                idempotency_key: Some("review-project-19-sha"),
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor: 100,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(initial.lifecycle, PromptTaskLifecycle::AwaitingPriorSettle);
        let expected_turn_id = runtime_turn_id(task_id, "dispatch");
        assert_eq!(initial.turn_id.as_deref(), Some(expected_turn_id.as_str()));
        assert!(!initial.exact_native_turn);

        let mut task = initial;
        assert!(apply_task_event(
            &mut task,
            &json!({"cursor": 101, "pane_id": "w1:p1", "agent_status": "done"})
        ));
        assert_eq!(task.lifecycle, PromptTaskLifecycle::AwaitingActivity);
        assert!(task.prior_settle_observed);
        assert!(!task.is_terminal());

        assert!(apply_task_event(
            &mut task,
            &json!({"cursor": 102, "pane_id": "w1:p1", "agent_status": "working"})
        ));
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Running);
        assert!(task.activity_observed);

        assert!(apply_task_event(
            &mut task,
            &json!({"cursor": 103, "pane_id": "w1:p1", "agent_status": "done"})
        ));
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert!(task.is_terminal());
        registry.save_task(&task, None).unwrap();
        drop(registry);

        let reopened =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&db_path).unwrap())));
        let status = reopened.task_status(task_id);
        assert_eq!(status["ok"], true);
        assert_eq!(status["task"]["lifecycle"], "completed");
        assert_eq!(status["task"]["parent_target"], "parent");
        assert_eq!(status["task"]["turn_correlation"], "runtime_activity_gate");
        let found = task_call_with_parent_session(
            &reopened,
            AGENT_TASK_STATUS_METHOD,
            &json!({"idempotency_key": "review-project-19-sha", "target": "child"}),
            None,
        );
        assert_eq!(found["task"]["task_id"], task_id);
        assert_eq!(found["task"]["lifecycle"], "completed");
        assert!(found["task"].get("idempotency_key_digest").is_none());
        // Reusing a key after its short-lived replay reservation expires must
        // not dispatch new work while the durable task still owns the key.
        for fingerprint in ["fingerprint", "different-work"] {
            let reused = begin_persisted(
                reopened.inner.store.as_ref().unwrap(),
                "review-project-19-sha",
                fingerprint,
            )
            .unwrap_err();
            assert_eq!(reused["code"], "agent_task_idempotency_key_in_use");
        }
        assert_eq!(
            reopened.task_status_by_idempotency("review-project-19-sha", "wrong-target", None)["code"],
            "agent_task_not_found"
        );
        assert_eq!(
            reopened.task_status_by_idempotency("review-project-19-sha", "child", Some("wrong"))["code"],
            "agent_task_not_found"
        );

        let inbox = reopened.task_inbox(None, Some("parent"), None, false, true, 10);
        assert_eq!(inbox["summary"]["completed"], 1);
        assert_eq!(inbox["tasks"][0]["task_id"], task_id);
        assert_eq!(inbox["attention"]["attempted"], true);
        assert_eq!(inbox["attention"]["used"], false);
        assert_eq!(inbox["attention"]["reason"], "not_configured");
        assert_eq!(inbox["attention"]["children"][0]["task_id"], task_id);
        assert!(
            inbox["attention"]["children"][0]
                .get("dispatch_id")
                .is_none()
        );
        assert!(inbox["attention"]["children"][0].get("status").is_none());
        assert_eq!(inbox["attention"]["metrics"]["question_count"], 1);
        assert_eq!(inbox["attention"]["metrics"]["budget_ms"], 1_500);
        assert!(
            inbox["attention"]["metrics"]["state_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes < 1024)
        );
        assert_eq!(reopened.task_ack(task_id)["ok"], true);
        let after_ack = reopened.task_inbox(None, Some("parent"), None, false, true, 10);
        assert_eq!(after_ack["summary"]["completed"], 0);
        assert!(after_ack["tasks"].as_array().unwrap().is_empty());
        assert_eq!(after_ack["attention"]["reason"], "no_pending_terminal");

        // Older task rows did not store a key digest; the stable task_id
        // must still fence a replay after the shorter reservation expires.
        let mut legacy = task.clone();
        legacy.task_id = dispatch_id(Some("legacy-review-key"), "legacy-fingerprint");
        legacy.dispatch_id = legacy.task_id.clone();
        legacy.idempotency_key_digest = None;
        reopened.save_task(&legacy, None).unwrap();
        let reused_legacy = begin_persisted(
            reopened.inner.store.as_ref().unwrap(),
            "legacy-review-key",
            "legacy-fingerprint",
        )
        .unwrap_err();
        assert_eq!(reused_legacy["code"], "agent_task_idempotency_key_in_use");

        // Recover even when the original task is older than the bounded inbox.
        for index in 0..520 {
            let mut filler = task.clone();
            filler.task_id = format!("dispatch:prompt:filler-{index}");
            filler.dispatch_id = filler.task_id.clone();
            filler.idempotency_key_digest = None;
            reopened.save_task(&filler, None).unwrap();
        }
        let recovered = reopened.task_status_by_idempotency("review-project-19-sha", "child", None);
        assert_eq!(recovered["task"]["task_id"], task_id);

        let mut duplicate = task.clone();
        duplicate.task_id = "dispatch:prompt:duplicate".to_owned();
        duplicate.dispatch_id = duplicate.task_id.clone();
        // The existing unique task-key index rejects a second owner.
        assert!(reopened.save_task(&duplicate, None).is_err());
        assert_eq!(
            reopened.task_status_by_idempotency("review-project-19-sha", "child", None)["task"]["task_id"],
            task_id
        );

        let mut session_bound = task.clone();
        session_bound.task_id = "dispatch:prompt:session-bound".to_owned();
        session_bound.dispatch_id = session_bound.task_id.clone();
        session_bound.parent_session_ref = Some("session:owner".to_owned());
        session_bound.idempotency_key_digest = Some(stable_digest(&[
            b"herdr_prompt/task_key",
            b"separate-session-key",
        ]));
        reopened.save_task(&session_bound, None).unwrap();
        assert_eq!(
            reopened.task_status_by_idempotency("separate-session-key", "child", None)["code"],
            "agent_task_not_found"
        );
        assert_eq!(
            reopened.task_status_by_idempotency(
                "separate-session-key",
                "child",
                Some("session:owner")
            )["task"]["task_id"],
            session_bound.task_id
        );

        drop(reopened);
        fs::remove_file(&db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        fs::remove_dir_all(&semantic_config_dir).ok();
    }

    #[test]
    fn awaiting_activity_times_out_to_durable_delivery_uncertain_terminal() {
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-timeout-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry::with_store(store);
        let idle = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("idle".to_owned()),
            state_change_seq: Some(20),
        };
        let task_id = "dispatch:prompt:activity-timeout-test";
        let mut task = registry
            .register_task(RegisterTaskInput {
                task_id,
                target: "child",
                fingerprint: "fingerprint",
                idempotency_key: None,
                before: Some(&idle),
                after: Some(&idle),
                submitted: Some(true),
                start_cursor: 200,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(task.lifecycle, PromptTaskLifecycle::AwaitingActivity);
        task.updated_at_ms = now_ms()
            .saturating_sub(PROMPT_TASK_ACTIVITY_START_TIMEOUT_MS)
            .saturating_sub(1);
        registry.save_task(&task, None).unwrap();

        let reconciled = registry.reconcile_task(task_id).unwrap().unwrap();
        assert_eq!(reconciled.lifecycle, PromptTaskLifecycle::DeliveryUncertain);
        assert!(reconciled.is_terminal());
        assert_eq!(
            reconciled.terminal_status.as_deref(),
            Some("delivery_uncertain")
        );
        assert_eq!(
            reconciled.notification_error.as_deref(),
            Some("activity_start_timeout")
        );

        let inbox = registry.task_inbox(None, Some("parent"), None, false, true, 10);
        assert_eq!(inbox["summary"]["running"], 0);
        assert_eq!(inbox["summary"]["uncertain"], 1);
        assert_eq!(inbox["tasks"][0]["lifecycle"], "delivery_uncertain");

        drop(registry);
        fs::remove_file(&db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }

    #[test]
    fn status_wait_timeout_keeps_dispatch_and_delivery_evidence() {
        let socket = temp_socket();
        let client = HerdrClient::new(&socket);
        let before = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("idle".to_owned()),
            state_change_seq: Some(40),
        };
        let result = prompt_failure(
            &client,
            "pi",
            Some(&before),
            true,
            HerdrError {
                code: "timeout".to_owned(),
                message: "timed out waiting for agent status".to_owned(),
            },
            "dispatch:prompt:test-timeout",
        );
        assert_eq!(result["failure_phase"], "post_submission_status_wait");
        assert_eq!(result["wait"]["completed"], false);
        assert_eq!(result["dispatch_id"], "dispatch:prompt:test-timeout");
        assert_eq!(result["dispatch_correlation"]["mode"], "awaiting_activity");
        assert_eq!(result["dispatch_correlation"]["terminal"], false);
        assert_eq!(result["submitted"], "unknown");
        assert_eq!(result["delivery_uncertain"], true);
    }

    #[test]
    fn unresolved_agent_target_is_pre_dispatch_not_human_boundary() {
        let socket = temp_socket();
        let client = HerdrClient::new(&socket);
        let result = prompt_failure(
            &client,
            "w1:p9",
            None,
            false,
            HerdrError {
                code: "agent_not_found".to_owned(),
                message: "no agent is bound to pane w1:p9".to_owned(),
            },
            "dispatch:prompt:missing-agent",
        );
        assert_eq!(result["failure_phase"], "resolve");
        assert_eq!(result["submitted"], false);
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["requires_human"], false);
        assert_eq!(result["retryable"], false);
        assert_eq!(result["recovery"]["action"], "start_agent_then_reprompt");
        assert_eq!(result["recovery"]["start_method"], "agent.start");
        assert_eq!(result["recovery"]["split_method"], "pane.split");
        assert!(result.get("delivery_uncertain").is_none());
    }

    /// user owner | Given a nonterminal task persisted under another boot
    /// | When a new runtime reconciles it | Then it fails closed as
    /// delivery_uncertain (runtime_boot_changed / runtime_boot_unknown);
    /// same-boot done events still complete normally.
    #[test]
    fn cross_boot_and_legacy_tasks_fail_closed_on_reconcile() {
        let _guard = crate::test_env::lock();
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-boot-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let cache = Arc::new(EventCache::from_snapshot_for_test(json!({"panes": []})));
        let registry = PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(Arc::clone(&store)),
                cache: Some(Arc::clone(&cache)),
                client: None,
            }),
        };
        let before = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("idle".to_owned()),
            state_change_seq: Some(1),
        };
        let after = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("working".to_owned()),
            state_change_seq: Some(2),
        };
        let initial = registry
            .register_task(RegisterTaskInput {
                task_id: "dispatch:prompt:boot-fence",
                target: "child",
                fingerprint: "fp",
                idempotency_key: None,
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor: 100,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
            })
            .unwrap()
            .unwrap();
        assert!(initial.boot_epoch.is_some());

        // Cross-boot: persisted epoch differs from the live cache's boot_id.
        let mut cross = initial.clone();
        cross.boot_epoch = Some("old-boot".to_owned());
        registry.save_task(&cross, None).unwrap();
        let fenced = registry
            .reconcile_task("dispatch:prompt:boot-fence")
            .unwrap()
            .unwrap();
        assert_eq!(
            fenced.terminal_status.as_deref(),
            Some("delivery_uncertain")
        );
        assert_eq!(
            fenced.notification_error.as_deref(),
            Some("runtime_boot_changed")
        );

        // Legacy record without an epoch.
        let mut legacy = initial.clone();
        legacy.task_id = "dispatch:prompt:boot-legacy".to_owned();
        legacy.dispatch_id = legacy.task_id.clone();
        legacy.boot_epoch = None;
        registry.save_task(&legacy, None).unwrap();
        let closed = registry
            .reconcile_task("dispatch:prompt:boot-legacy")
            .unwrap()
            .unwrap();
        assert_eq!(
            closed.terminal_status.as_deref(),
            Some("delivery_uncertain")
        );
        assert_eq!(
            closed.notification_error.as_deref(),
            Some("runtime_boot_unknown")
        );

        // Same-boot done event still completes normally.
        let mut live = initial.clone();
        live.task_id = "dispatch:prompt:boot-same".to_owned();
        live.dispatch_id = live.task_id.clone();
        registry.save_task(&live, None).unwrap();
        assert!(apply_task_event(
            &mut live,
            &json!({"cursor": 101, "pane_id": "w1:p1", "agent_status": "done"})
        ));
        assert_eq!(live.lifecycle, PromptTaskLifecycle::Completed);
        assert!(live.is_terminal());
    }

    fn escalation_registry(tag: &str) -> (PathBuf, PromptRegistry) {
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-escalation-{}-{}-{}.sqlite",
            tag,
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        (db_path, PromptRegistry::with_store(store))
    }

    fn remove_escalation_db(db_path: &std::path::Path) {
        fs::remove_file(db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }

    /// Durable task fixture with a controlled activity age, so escalation
    /// thresholds can be exercised through the real `task_inbox` entrypoint.
    fn escalation_task(
        task_id: &str,
        target: &str,
        lifecycle: PromptTaskLifecycle,
        age_ms: u64,
    ) -> PromptTaskRecord {
        let now = now_ms();
        let terminal = lifecycle.is_terminal();
        PromptTaskRecord {
            schema_version: 1,
            task_id: task_id.to_owned(),
            dispatch_id: task_id.to_owned(),
            target: target.to_owned(),
            idempotency_key_digest: None,
            pane_id: Some("w1:p1".to_owned()),
            workspace_id: Some("w1".to_owned()),
            baseline_status: None,
            baseline_seq: None,
            start_cursor: 0,
            last_cursor: 0,
            activity_observed: lifecycle != PromptTaskLifecycle::AwaitingActivity,
            prior_settle_observed: false,
            lifecycle: lifecycle.clone(),
            turn_id: None,
            exact_native_turn: false,
            terminal_status: terminal.then(|| lifecycle.as_str().to_owned()),
            terminal_cursor: None,
            terminal_at_ms: terminal.then(|| now.saturating_sub(age_ms)),
            last_heartbeat_at_ms: None,
            worker_done_at_ms: None,
            worker_done_summary: None,
            stall_after_ms: None,
            final_excerpt: None,
            awaiting_children: None,
            parent_target: Some("parent".to_owned()),
            parent_session_ref: None,
            boot_epoch: None,
            notification_state: if terminal {
                "delivered".to_owned()
            } else {
                "pending".to_owned()
            },
            notification_attempts: 0,
            notification_at_ms: None,
            notification_error: None,
            acknowledged_at_ms: None,
            created_at_ms: now.saturating_sub(age_ms),
            updated_at_ms: now.saturating_sub(age_ms),
        }
    }

    fn worker_registry(tag: &str) -> (PathBuf, PromptRegistry, Arc<AtomicU64>) {
        let (path, mut registry) = task_registry(tag);
        let clock = Arc::new(AtomicU64::new(now_ms()));
        Arc::get_mut(&mut registry.inner).unwrap().clock = Some(clock.clone());
        (path, registry, clock)
    }

    /// user worker | Given a durable running task with distinct ids | When
    /// completion reports race and the runtime reloads | Then exactly one
    /// succeeds, mismatches fail, and duplicate reports change no stored fact.
    #[test]
    fn worker_reports_are_durable_exactly_once_and_identity_bound() {
        let (path, registry, clock) = worker_registry("reports");
        let mut task = escalation_task("report-task", "child", PromptTaskLifecycle::Running, 0);
        task.dispatch_id = "distinct-dispatch".to_owned();
        registry.save_task(&task, None).unwrap();
        let report = |params| {
            task_call_with_parent_session(&registry, AGENT_TASK_REPORT_METHOD, &params, None)
        };
        for params in [
            json!({"task_id": "missing", "dispatch_id": task.dispatch_id, "kind": "worker_done"}),
            json!({"task_id": task.task_id, "dispatch_id": task.task_id, "kind": "worker_done"}),
        ] {
            assert_eq!(report(params)["code"], "task_identity_mismatch");
            assert_eq!(registry.load_task(&task.task_id).unwrap().unwrap(), task);
        }
        assert_eq!(
            report(
                json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "worker_done", "summary": "界".repeat(683)})
            )["code"],
            "invalid_params"
        );
        let params = json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "worker_done", "summary": "done\n\u{001b}\t✓"});
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let registry = registry.clone();
                let params = params.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    task_call_with_parent_session(
                        &registry,
                        AGENT_TASK_REPORT_METHOD,
                        &params,
                        None,
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|result| result["ok"] == true).count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result["code"] == "worker_done_already_reported")
                .count(),
            7
        );
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(stored.worker_done_at_ms, Some(clock.load(Ordering::SeqCst)));
        assert_eq!(stored.worker_done_summary.as_deref(), Some("done✓"));
        let status = registry.task_status(&task.task_id);
        assert_eq!(status["task"]["lifecycle"], "completed");
        assert_eq!(status["task"]["terminal_status"], "worker_done");
        let reopened =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&path).unwrap())));
        assert_eq!(
            task_call_with_parent_session(&reopened, AGENT_TASK_REPORT_METHOD, &params, None)["code"],
            "worker_done_already_reported"
        );
        assert_eq!(
            report(
                json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "heartbeat"})
            )["code"],
            "agent_task_terminal"
        );
        assert_eq!(registry.load_task(&task.task_id).unwrap().unwrap(), stored);
        drop(reopened);
        drop(registry);
        remove_task_db(&path);
    }

    /// user worker | Given a task already settled by lifecycle | When a late
    /// worker_done arrives | Then only the report fact is added, with no second
    /// parent notification and no lifecycle, terminal, excerpt or ack reset.
    #[test]
    fn late_worker_done_preserves_lifecycle_and_parent_notification() {
        let (path, registry, _) = worker_registry("late-report");
        for lifecycle in [
            PromptTaskLifecycle::Completed,
            PromptTaskLifecycle::Blocked,
            PromptTaskLifecycle::Failed,
            PromptTaskLifecycle::DeliveryUncertain,
        ] {
            let mut task = escalation_task(lifecycle.as_str(), "child", lifecycle.clone(), 1000);
            task.notification_attempts = 1;
            task.acknowledged_at_ms = Some(task.updated_at_ms);
            task.final_excerpt = Some("existing output".to_owned());
            registry.save_task(&task, None).unwrap();
            let report = task_call_with_parent_session(
                &registry,
                AGENT_TASK_REPORT_METHOD,
                &json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "worker_done"}),
                None,
            );
            assert_eq!(report["ok"], true, "{report}");
            let mut expected = task;
            expected.worker_done_at_ms = Some(registry.now_ms());
            expected.updated_at_ms = registry.now_ms();
            assert_eq!(
                registry.load_task(&expected.task_id).unwrap().unwrap(),
                expected
            );
        }
        drop(registry);
        remove_task_db(&path);
    }

    /// user parent | Given old stored JSON without worker fields and a fake
    /// clock | When heartbeats and progress stop for the configured interval |
    /// Then status and both inbox views agree on non-terminal stalled liveness.
    #[test]
    fn old_tasks_and_heartbeats_share_stall_liveness_with_inbox() {
        let (path, registry, clock) = worker_registry("liveness");
        let old_json = include_str!("../tests/fixtures/prompt-task-pre-worker-report.json");
        let task: PromptTaskRecord = serde_json::from_str(old_json).unwrap();
        clock.store(task.updated_at_ms, Ordering::SeqCst);
        assert!(task.last_heartbeat_at_ms.is_none());
        assert!(task.worker_done_at_ms.is_none());
        assert!(task.worker_done_summary.is_none());
        assert!(task.stall_after_ms.is_none());
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(ESCALATION_STALL_IDLE_MS - 1, Ordering::SeqCst);
        assert_eq!(
            registry.task_status(&task.task_id)["task"]["liveness"],
            "active"
        );
        clock.fetch_add(1, Ordering::SeqCst);
        let status = registry.task_status(&task.task_id);
        assert_eq!(status["task"]["liveness"], "stalled");
        assert_eq!(status["task"]["terminal"], false);
        let inbox = registry.task_inbox_with_running(None, "session-worker", 10);
        assert_eq!(inbox["tasks"][0]["liveness"], "stalled");
        assert_eq!(inbox["escalation"]["signals"][0]["kind"], "stall_suspected");
        assert_eq!(
            apply_inbox_view(inbox, InboxView::Compact)["tasks"][0]["liveness"],
            "stalled"
        );
        let before = registry.load_task(&task.task_id).unwrap().unwrap();
        let heartbeat = task_call_with_parent_session(
            &registry,
            AGENT_TASK_REPORT_METHOD,
            &json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "heartbeat"}),
            None,
        );
        assert_eq!(heartbeat["ok"], true);
        assert_eq!(heartbeat["task"]["liveness"], "active");
        let after = registry.load_task(&task.task_id).unwrap().unwrap();
        let mut expected = before;
        expected.last_heartbeat_at_ms = Some(registry.now_ms());
        expected.updated_at_ms = registry.now_ms();
        assert_eq!(after, expected, "heartbeat only updates its two timestamps");
        assert!(
            registry.task_inbox(None, None, None, false, false, 10)["escalation"]["signals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let mut custom = after;
        custom.stall_after_ms = Some(60_000);
        registry.save_task(&custom, None).unwrap();
        clock.fetch_add(59_999, Ordering::SeqCst);
        assert_eq!(
            registry.task_status(&task.task_id)["task"]["liveness"],
            "active"
        );
        clock.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            registry.task_status(&task.task_id)["task"]["liveness"],
            "stalled"
        );
        drop(registry);
        remove_task_db(&path);
    }

    /// user parent | Given a submitted running task | When dispatch wait expires
    /// and is repeated with the same key | Then both return successful
    /// checkpoints, preserve task evidence and submit only once.
    #[test]
    fn dispatch_wait_timeout_is_a_checkpoint_without_resubmission() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let texts = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_dispatch_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}}}),
                json!({"result": {"prompt": {"status": "submitted", "agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 2}}}}),
            ],
            texts.clone(),
        );
        let client = HerdrClient::new(&socket);
        let (path, registry, _) = worker_registry("checkpoint");
        for value in [json!(59_999), json!(7_200_001), json!("60000")] {
            assert_eq!(
                run(
                    &client,
                    &registry,
                    &json!({"target": "w1:p1", "text": "work", "stall_after_ms": value})
                )["code"],
                "invalid_params"
            );
        }
        let mut args = json!({"target": "w1:p1", "text": "work", "idempotency_key": "checkpoint", "stall_after_ms": 60_000, "wait": {"timeout_ms": 1}});
        let first = run(&client, &registry, &args);
        server.join().unwrap();
        args["stall_after_ms"] = json!(7_200_000);
        let replay = run(&client, &registry, &args);
        for result in [&first, &replay] {
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["wait"]["completed"], false);
            assert_eq!(result["wait"]["outcome"], "checkpoint");
            assert!(result.get("failure").is_none());
            assert_eq!(result["task"]["lifecycle"], "running");
            assert_eq!(result["task"]["liveness"], "active");
            assert_eq!(
                result["task"]["stall_after_ms"], 60_000,
                "replay retains the dispatch threshold"
            );
            assert_eq!(result["task_id"], first["task_id"]);
            assert_eq!(result["dispatch_id"], first["dispatch_id"]);
        }
        assert_eq!(texts.lock().unwrap().len(), 1);
        assert_eq!(replay["idempotent_replay"], true);
        let mut failure =
            json!({"ok": false, "failure": "delivery_failed", "delivery_state": "not_delivered"});
        apply_task_wait(
            &registry,
            &mut failure,
            first["task_id"].as_str().unwrap(),
            &json!({"timeout_ms": 0}),
        );
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["failure"], "delivery_failed");
        let mut missing = json!({"ok": true});
        apply_task_wait(
            &registry,
            &mut missing,
            "unknown-task",
            &json!({"timeout_ms": 0}),
        );
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["failure"], "agent_task_not_found");
        drop(registry);
        remove_task_db(&path);
        fs::remove_file(socket).unwrap();
    }

    /// user parent | Given one target/parent lane of durable tasks in the
    /// six-hour window | When the inbox is read | Then four dispatches yield
    /// one deterministic consult_advisor signal, three yield none, and tasks
    /// older than the window never count.
    #[test]
    fn escalation_reports_repair_budget_exhausted_at_four_dispatches() {
        let (db_path, registry) = escalation_registry("budget");
        for (index, age_ms) in [3_000_u64, 2_000, 1_000].into_iter().enumerate() {
            let task = escalation_task(
                &format!("dispatch:prompt:budget-{index}"),
                "child",
                PromptTaskLifecycle::Completed,
                age_ms,
            );
            registry.save_task(&task, None).unwrap();
        }

        let three = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        assert_eq!(three["escalation"]["advisory_only"], true);
        assert_eq!(three["escalation"]["redispatch"], "never_automatic");
        assert!(
            three["escalation"]["signals"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let fourth = escalation_task(
            "dispatch:prompt:budget-3",
            "child",
            PromptTaskLifecycle::Completed,
            500,
        );
        registry.save_task(&fourth, None).unwrap();
        // An old attempt must stay out of the live escalation window.
        let old = escalation_task(
            "dispatch:prompt:budget-old",
            "child",
            PromptTaskLifecycle::Completed,
            7 * 60 * 60_000,
        );
        registry.save_task(&old, None).unwrap();

        let inbox = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        let signals = inbox["escalation"]["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "repair_budget_exhausted");
        assert_eq!(signals[0]["recommended_action"], "consult_advisor");
        assert_eq!(signals[0]["target"], "child");
        assert_eq!(signals[0]["parent_session_ref_present"], false);
        assert_eq!(signals[0]["dispatch_count"], 4);
        assert_eq!(signals[0]["task_ids"][0], "dispatch:prompt:budget-3");
        assert_eq!(signals[0]["task_ids"].as_array().unwrap().len(), 4);

        // Signals stay ordered by (kind, then target) across lanes.
        for (index, age_ms) in [200_u64, 100].into_iter().enumerate() {
            let other = escalation_task(
                &format!("dispatch:prompt:other-{index}"),
                "other",
                PromptTaskLifecycle::Failed,
                age_ms,
            );
            registry.save_task(&other, None).unwrap();
        }
        let ordered = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        let signals = ordered["escalation"]["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0]["kind"], "repair_budget_exhausted");
        assert_eq!(signals[0]["target"], "child");
        assert_eq!(signals[1]["kind"], "unsuccessful_streak");
        assert_eq!(signals[1]["target"], "other");
        assert_eq!(signals[1]["streak_length"], 2);

        drop(registry);
        remove_escalation_db(&db_path);
    }

    /// user parent | Given a lane whose newest terminal tasks are failed and
    /// blocked | When the inbox is read | Then one consult_advisor streak
    /// signal appears without exposing the raw parent session reference.
    #[test]
    fn escalation_reports_unsuccessful_streak_without_exposing_session_refs() {
        let (db_path, registry) = escalation_registry("streak");
        for (task_id, lifecycle, age_ms) in [
            ("streak-failed", PromptTaskLifecycle::Failed, 1_000_u64),
            ("streak-blocked", PromptTaskLifecycle::Blocked, 2_000),
        ] {
            let mut task = escalation_task(
                &format!("dispatch:prompt:{task_id}"),
                "child",
                lifecycle,
                age_ms,
            );
            task.parent_session_ref = Some("session:private-owner".to_owned());
            registry.save_task(&task, None).unwrap();
        }

        let inbox = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        let signals = inbox["escalation"]["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "unsuccessful_streak");
        assert_eq!(signals[0]["recommended_action"], "consult_advisor");
        assert_eq!(signals[0]["parent_session_ref_present"], true);
        assert_eq!(signals[0]["streak_length"], 2);
        assert_eq!(
            signals[0]["task_ids"],
            json!([
                "dispatch:prompt:streak-failed",
                "dispatch:prompt:streak-blocked"
            ])
        );
        assert!(signals[0].get("parent_session_ref").is_none());
        // The escalation object never echoes the raw reference; the sibling
        // `tasks` array keeps its pre-existing caller-scoped field.
        assert!(
            !inbox["escalation"]
                .to_string()
                .contains("session:private-owner")
        );

        // Same facts through the MCP-facing method router a caller really uses.
        let routed = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "limit": 10}),
            Some("session:private-owner"),
        );
        assert_eq!(routed["escalation"]["redispatch"], "never_automatic");
        assert_eq!(
            routed["escalation"]["signals"][0]["kind"],
            "unsuccessful_streak"
        );
        assert_eq!(routed["escalation"]["signals"][0]["streak_length"], 2);

        drop(registry);
        remove_escalation_db(&db_path);
    }

    /// user parent | Given a delivery-uncertain task newer than failed work
    /// | When the inbox is read | Then uncertainty neither breaks a streak
    /// into a failure nor ever produces a consult_advisor hint.
    #[test]
    fn escalation_never_counts_delivery_uncertainty_as_failure() {
        let (db_path, registry) = escalation_registry("uncertain");
        for (task_id, lifecycle, age_ms) in [
            (
                "uncertain-newest",
                PromptTaskLifecycle::DeliveryUncertain,
                500_u64,
            ),
            ("uncertain-failed", PromptTaskLifecycle::Failed, 1_000),
            ("uncertain-blocked", PromptTaskLifecycle::Blocked, 2_000),
        ] {
            let task = escalation_task(
                &format!("dispatch:prompt:{task_id}"),
                "child",
                lifecycle,
                age_ms,
            );
            registry.save_task(&task, None).unwrap();
        }

        let inbox = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        let signals = inbox["escalation"]["signals"].as_array().unwrap();
        assert!(
            signals
                .iter()
                .all(|signal| signal["kind"] != "unsuccessful_streak")
        );
        assert!(
            signals
                .iter()
                .all(|signal| signal["recommended_action"] != "consult_advisor")
        );
        assert!(!inbox["escalation"].to_string().contains("uncertain-newest"));
        assert_eq!(inbox["escalation"]["redispatch"], "never_automatic");

        drop(registry);
        remove_escalation_db(&db_path);
    }

    /// user parent | Given a running task with no observed activity | When
    /// the inbox is read | Then only the 25-minute-old task yields one
    /// verify_progress stall signal while a 10-minute-old task yields none.
    #[test]
    fn escalation_reports_stall_only_after_twenty_idle_minutes() {
        let (db_path, registry) = escalation_registry("stall");
        let fresh = escalation_task(
            "dispatch:prompt:stall-fresh",
            "child",
            PromptTaskLifecycle::Running,
            10 * 60_000,
        );
        registry.save_task(&fresh, None).unwrap();
        let quiet = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        assert!(
            quiet["escalation"]["signals"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let stale = escalation_task(
            "dispatch:prompt:stall-stale",
            "child",
            PromptTaskLifecycle::Running,
            25 * 60_000,
        );
        registry.save_task(&stale, None).unwrap();

        let inbox = registry.task_inbox(None, Some("parent"), None, false, false, 10);
        let signals = inbox["escalation"]["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "stall_suspected");
        assert_eq!(signals[0]["recommended_action"], "verify_progress");
        assert_eq!(signals[0]["task_id"], "dispatch:prompt:stall-stale");
        assert_eq!(signals[0]["lifecycle"], "running");
        assert!(
            signals[0]["idle_ms"].as_u64().unwrap() >= 20 * 60_000,
            "idle_ms must report the observed idle window"
        );

        drop(registry);
        remove_escalation_db(&db_path);
    }

    fn task_registry(tag: &str) -> (PathBuf, PromptRegistry) {
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-inbox-{}-{}-{}.sqlite",
            tag,
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        (db_path, PromptRegistry::with_store(store))
    }

    fn remove_task_db(db_path: &std::path::Path) {
        fs::remove_file(db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }

    /// user parent | Given an unacknowledged terminal task already in scope |
    /// When the inbox is read with a long `wait_ms` | Then it returns
    /// immediately with `woke: terminal_task` instead of blocking.
    #[test]
    fn inbox_wait_returns_immediately_when_an_unacknowledged_terminal_task_exists() {
        let (db_path, registry) = task_registry("wait-ready");
        let task = escalation_task(
            "dispatch:prompt:ready",
            "child",
            PromptTaskLifecycle::Completed,
            1_000,
        );
        registry.save_task(&task, None).unwrap();

        let started = std::time::Instant::now();
        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "wait_ms": 60_000}),
            None,
        );
        let elapsed = started.elapsed();

        assert_eq!(inbox["ok"], true);
        assert_eq!(inbox["wait"]["woke"], "terminal_task");
        assert!(
            inbox["wait"]["waited_ms"].as_u64().unwrap() < 1_000,
            "an already-pending inbox must not consume the wait budget"
        );
        assert_eq!(inbox["tasks"][0]["task_id"], "dispatch:prompt:ready");
        assert!(elapsed < Duration::from_secs(5));

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user parent | Given only a running child task | When the inbox waits and
    /// the task turns terminal during the wait | Then it wakes on the new
    /// terminal fact instead of waiting for the timeout.
    #[test]
    fn inbox_wait_wakes_when_a_running_task_completes() {
        let (db_path, registry) = task_registry("wait-wakes");
        let running = escalation_task(
            "dispatch:prompt:slow",
            "child",
            PromptTaskLifecycle::Running,
            0,
        );
        registry.save_task(&running, None).unwrap();

        let mut terminal = running.clone();
        terminal.lifecycle = PromptTaskLifecycle::Completed;
        terminal.terminal_status = Some("completed".to_owned());
        terminal.terminal_at_ms = Some(now_ms());
        terminal.updated_at_ms = now_ms();
        terminal.notification_state = "delivered".to_owned();
        let writer = registry.clone();
        let writer_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            writer.save_task(&terminal, None).unwrap();
        });

        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "wait_ms": 800}),
            None,
        );
        writer_handle.join().unwrap();

        assert_eq!(inbox["wait"]["woke"], "terminal_task");
        assert!(
            inbox["wait"]["waited_ms"].as_u64().unwrap() >= 40,
            "waited_ms must report the observed wait window"
        );
        assert_eq!(inbox["tasks"][0]["task_id"], "dispatch:prompt:slow");
        assert_eq!(inbox["tasks"][0]["lifecycle"], "completed");

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user parent | Given a running task that never settles | When the inbox
    /// waits | Then it times out with `woke: timeout` and the same task facts a
    /// non-waiting read would return.
    #[test]
    fn inbox_wait_times_out_with_the_same_task_facts() {
        let (db_path, registry) = task_registry("wait-timeout");
        let running = escalation_task(
            "dispatch:prompt:still-running",
            "child",
            PromptTaskLifecycle::Running,
            0,
        );
        registry.save_task(&running, None).unwrap();

        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "wait_ms": 120}),
            None,
        );
        let plain = registry.task_inbox(None, Some("parent"), None, false, false, 50);

        assert_eq!(inbox["wait"]["woke"], "timeout");
        assert!(inbox["wait"]["waited_ms"].as_u64().unwrap() >= 120);
        assert_eq!(inbox["summary"], plain["summary"]);
        assert_eq!(inbox["tasks"], plain["tasks"]);
        assert_eq!(inbox["escalation"], plain["escalation"]);
        assert_eq!(inbox["summary"]["running"], 1);
        assert!(inbox["tasks"].as_array().unwrap().is_empty());

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user parent | Given a `wait_ms` outside 1..=60000 | When the inbox is
    /// read | Then the call is rejected with the existing invalid-params style.
    #[test]
    fn inbox_rejects_wait_ms_outside_the_bounded_range() {
        let registry = PromptRegistry::new();
        for wait_ms in [
            json!(0),
            json!(60_001),
            json!("300"),
            json!(1.5),
            json!(null),
        ] {
            let inbox = task_call_with_parent_session(
                &registry,
                AGENT_TASK_INBOX_METHOD,
                &json!({"wait_ms": wait_ms}),
                None,
            );
            assert_eq!(inbox["ok"], false, "wait_ms={wait_ms} must be rejected");
            assert_eq!(inbox["code"], "invalid_params");
            assert!(
                inbox["message"].as_str().unwrap().contains("wait_ms"),
                "rejection must name wait_ms"
            );
        }
    }

    /// user parent | Given a terminal task | When the inbox is read with
    /// `view: compact` | Then each task keeps only the projection fields, never
    /// the raw parent session reference, while the full view is unchanged.
    #[test]
    fn inbox_compact_view_projects_listed_fields_only() {
        let (db_path, registry) = task_registry("compact");
        let mut task = escalation_task(
            "dispatch:prompt:compact",
            "child",
            PromptTaskLifecycle::Completed,
            100,
        );
        task.parent_session_ref = Some("session:private-owner".to_owned());
        task.final_excerpt = Some("DONE: report ready".to_owned());
        registry.save_task(&task, None).unwrap();

        let compact = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "view": "compact"}),
            Some("session:private-owner"),
        );
        let projected = compact["tasks"][0].as_object().unwrap();
        let mut keys = projected.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "acknowledged",
                "dispatch_id",
                "final_excerpt",
                "lifecycle",
                "liveness",
                "pane_id",
                "target",
                "task_id",
                "terminal",
                "terminal_at_ms",
                "updated_at_ms",
            ]
        );
        assert_eq!(projected["task_id"], "dispatch:prompt:compact");
        assert_eq!(projected["target"], "child");
        assert_eq!(projected["pane_id"], "w1:p1");
        assert_eq!(projected["lifecycle"], "completed");
        assert_eq!(projected["terminal"], true);
        assert_eq!(projected["acknowledged"], false);
        assert!(projected["terminal_at_ms"].is_u64());
        assert!(projected["updated_at_ms"].is_u64());
        assert!(!compact.to_string().contains("session:private-owner"));
        assert_eq!(compact["summary"]["completed"], 1);
        assert_eq!(compact["escalation"]["redispatch"], "never_automatic");
        assert!(compact.get("wait").is_none());

        let full = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent"}),
            Some("session:private-owner"),
        );
        let full_task = &full["tasks"][0];
        assert_eq!(full_task["parent_session_ref"], "session:private-owner");
        assert_eq!(full_task["dispatch_id"], "dispatch:prompt:compact");
        assert!(full_task.as_object().unwrap().len() > 8);
        assert!(full.get("wait").is_none());

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user parent | Given an unknown `view` value | When the inbox is read |
    /// Then the call is rejected with the existing invalid-params style.
    #[test]
    fn inbox_rejects_an_unknown_view() {
        let registry = PromptRegistry::new();
        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"view": "brief"}),
            None,
        );
        assert_eq!(inbox["code"], "invalid_params");
        assert!(inbox["message"].as_str().unwrap().contains("view"));
    }

    /// user parent | Given in-scope, out-of-scope, and unknown task ids | When
    /// one batch ack is submitted | Then only in-scope ids are acked and the
    /// rest are reported as `not_found` in input order.
    #[test]
    fn batch_ack_acks_in_scope_ids_and_reports_the_rest_as_not_found() {
        let (db_path, registry) = task_registry("ack-batch");
        for (task_id, session) in [
            ("dispatch:prompt:ack-a", Some("session:parent")),
            ("dispatch:prompt:ack-b", Some("session:parent")),
            ("dispatch:prompt:ack-other", Some("session:other")),
        ] {
            let mut task = escalation_task(task_id, "child", PromptTaskLifecycle::Completed, 100);
            task.parent_session_ref = session.map(str::to_owned);
            registry.save_task(&task, None).unwrap();
        }

        let result = task_call_with_parent_session(
            &registry,
            AGENT_TASK_ACK_METHOD,
            &json!({"task_ids": [
                "dispatch:prompt:ack-b",
                "dispatch:prompt:ack-a",
                "dispatch:prompt:ack-other",
                "dispatch:prompt:unknown",
            ]}),
            Some("session:parent"),
        );
        assert_eq!(
            result,
            json!({
                "ok": true,
                "acked": ["dispatch:prompt:ack-b", "dispatch:prompt:ack-a"],
                "not_found": ["dispatch:prompt:ack-other", "dispatch:prompt:unknown"],
            })
        );
        for task_id in ["dispatch:prompt:ack-a", "dispatch:prompt:ack-b"] {
            assert!(
                registry
                    .load_task(task_id)
                    .unwrap()
                    .unwrap()
                    .acknowledged_at_ms
                    .is_some(),
                "{task_id} must be acknowledged"
            );
        }
        assert_eq!(
            registry
                .load_task("dispatch:prompt:ack-other")
                .unwrap()
                .unwrap()
                .acknowledged_at_ms,
            None,
            "an out-of-scope task must never be acked"
        );

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user parent | Given malformed `task_ids` batches | When a batch ack is
    /// submitted | Then every malformed batch is rejected before any mutation.
    #[test]
    fn batch_ack_rejects_empty_oversized_duplicate_or_mixed_batches() {
        let (db_path, registry) = task_registry("ack-invalid");
        let mut duplicate = escalation_task(
            "dispatch:prompt:dup",
            "child",
            PromptTaskLifecycle::Completed,
            100,
        );
        duplicate.parent_session_ref = Some("session:parent".to_owned());
        registry.save_task(&duplicate, None).unwrap();

        let oversized = (0..33)
            .map(|index| format!("dispatch:prompt:many-{index}"))
            .collect::<Vec<_>>();
        for params in [
            json!({"task_ids": []}),
            json!({"task_ids": oversized}),
            json!({"task_ids": ["dispatch:prompt:dup", "dispatch:prompt:dup"]}),
            json!({"task_id": "dispatch:prompt:dup", "task_ids": ["dispatch:prompt:dup"]}),
            json!({"task_ids": "dispatch:prompt:dup"}),
            json!({"task_ids": [7]}),
            json!({"task_ids": [""]}),
        ] {
            let result = task_call_with_parent_session(
                &registry,
                AGENT_TASK_ACK_METHOD,
                &params,
                Some("session:parent"),
            );
            assert_eq!(
                result["code"], "invalid_params",
                "{params} must be rejected"
            );
        }
        assert_eq!(
            registry
                .load_task("dispatch:prompt:dup")
                .unwrap()
                .unwrap()
                .acknowledged_at_ms,
            None,
            "a rejected batch must not acknowledge anything"
        );

        // The single-task path keeps today's response shape.
        let single = task_call_with_parent_session(
            &registry,
            AGENT_TASK_ACK_METHOD,
            &json!({"task_id": "dispatch:prompt:dup"}),
            Some("session:parent"),
        );
        assert_eq!(single["ok"], true);
        assert_eq!(single["task"]["task_id"], "dispatch:prompt:dup");
        assert_eq!(single["task"]["acknowledged"], true);
        assert!(single.get("acked").is_none());

        drop(registry);
        remove_task_db(&db_path);
    }
    fn counting_agent_server(
        listener: UnixListener,
        prompt_texts: Arc<Mutex<Vec<String>>>,
        state_probes: Arc<AtomicU64>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            use std::sync::atomic::Ordering;
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let mut response = match request["method"].as_str() {
                    Some("agent.prompt") => {
                        prompt_texts
                            .lock()
                            .unwrap()
                            .push(request["params"]["text"].as_str().unwrap_or("").to_owned());
                        json!({"error": {"code": "agent_not_found", "message": "agent target did not resolve"}})
                    }
                    _ => {
                        state_probes.fetch_add(1, Ordering::SeqCst);
                        json!({"result": {"agent": {
                            "pane_id": "w1:p2",
                            "agent_status": "working",
                            "state_change_seq": 5
                        }}})
                    }
                };
                response["id"] = request["id"].clone();
                writeln!(stream, "{response}").ok();
            }
        })
    }

    /// user parent | Given an acknowledged terminal task whose notification is
    /// still pending and a busy parent agent | When the inbox waits | Then the
    /// poll ticks stay on durable task facts instead of re-probing the parent on
    /// every tick; only the returned snapshot may notify.
    #[test]
    fn inbox_wait_poll_does_not_repeat_parent_notification_probes() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let prompt_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let state_probes = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = counting_agent_server(
            listener,
            Arc::clone(&prompt_texts),
            Arc::clone(&state_probes),
            Arc::clone(&stop),
        );

        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-inbox-probe-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: None,
                client: Some(HerdrClient::new(&socket)),
            }),
        };
        let mut task = escalation_task(
            "dispatch:prompt:pending-notify",
            "child",
            PromptTaskLifecycle::Completed,
            500,
        );
        task.acknowledged_at_ms = Some(now_ms());
        task.notification_state = "pending".to_owned();
        registry.save_task(&task, None).unwrap();

        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "parent", "wait_ms": 700}),
            None,
        );

        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::os::unix::net::UnixStream::connect(&socket);
        server.join().unwrap();

        assert_eq!(inbox["wait"]["woke"], "timeout");
        assert_eq!(
            state_probes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the returned snapshot may probe the parent agent"
        );
        assert!(
            prompt_texts.lock().unwrap().is_empty(),
            "a busy parent must not receive a notification"
        );
        assert_eq!(
            registry
                .load_task("dispatch:prompt:pending-notify")
                .unwrap()
                .unwrap()
                .notification_attempts,
            0
        );

        drop(registry);
        remove_task_db(&db_path);
        fs::remove_file(&socket).ok();
    }

    fn scripted_read_server(
        listener: UnixListener,
        responses: Vec<Value>,
        requests: Arc<Mutex<Vec<(String, Value)>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for mut response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                requests.lock().unwrap().push((
                    request["method"].as_str().unwrap_or_default().to_owned(),
                    request["params"].clone(),
                ));
                response["id"] = request["id"].clone();
                writeln!(stream, "{response}").unwrap();
            }
        })
    }

    fn layout_response(width: u16) -> Value {
        json!({"result": {"layout": {
            "panes": [{"pane_id": "w1:p1", "focused": true,
                       "rect": {"x": 0, "y": 0, "width": width, "height": 30}}],
            "area": {"x": 0, "y": 0, "width": 200, "height": 30},
        }}})
    }

    fn wide_layout_response() -> Value {
        layout_response(crate::pane_width::MIN_READABLE_PANE_COLS + 60)
    }

    fn excerpt_test_task(task_id: &str) -> PromptTaskRecord {
        PromptTaskRecord {
            schema_version: 1,
            task_id: task_id.to_owned(),
            dispatch_id: task_id.to_owned(),
            target: "child".to_owned(),
            idempotency_key_digest: None,
            pane_id: Some("w1:p1".to_owned()),
            workspace_id: Some("w1".to_owned()),
            baseline_status: Some("idle".to_owned()),
            baseline_seq: Some(1),
            start_cursor: 100,
            last_cursor: 100,
            lifecycle: PromptTaskLifecycle::Running,
            activity_observed: true,
            prior_settle_observed: true,
            turn_id: Some(runtime_turn_id(task_id, "dispatch")),
            exact_native_turn: false,
            terminal_status: None,
            terminal_cursor: None,
            terminal_at_ms: None,
            last_heartbeat_at_ms: None,
            worker_done_at_ms: None,
            worker_done_summary: None,
            stall_after_ms: None,
            final_excerpt: None,
            awaiting_children: None,
            parent_target: None,
            parent_session_ref: None,
            boot_epoch: Some("boot-excerpt-test".to_owned()),
            notification_state: "pending".to_owned(),
            notification_attempts: 0,
            notification_at_ms: None,
            notification_error: None,
            acknowledged_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    /// user owner | Given a running child whose pane reports done | When the
    /// terminal transition is reconciled | Then exactly one bounded native
    /// read is captured with ANSI stripped and trailing blanks trimmed.
    #[test]
    fn final_excerpt_is_captured_once_at_terminal_transition() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "analyzing module\n\u{1b}[32mprogress: 50%\u{1b}[0m\nDONE: report at /tmp/report.md\n\n\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                wide_layout_response(),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);

        let mut task = excerpt_test_task("dispatch:prompt:excerpt-once");
        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();
        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        let excerpt = task
            .final_excerpt
            .clone()
            .expect("terminal transition captures an excerpt");
        assert!(excerpt.contains("progress: 50%"));
        assert!(!excerpt.contains('\u{1b}'));
        assert!(excerpt.ends_with("DONE: report at /tmp/report.md\n"));

        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded.len(),
            4,
            "capture must verify, read, re-verify and probe the pane width"
        );
        assert_eq!(recorded[0].0, "agent.get");
        assert_eq!(recorded[0].1["target"], "w1:p1");
        assert_eq!(recorded[1].0, "agent.read");
        assert_eq!(recorded[1].1["target"], "w1:p1");
        assert_eq!(recorded[1].1["source"], "recent_unwrapped");
        assert_eq!(recorded[1].1["lines"], FINAL_EXCERPT_LINES);
        assert_eq!(recorded[1].1["strip_ansi"], true);
        assert_eq!(recorded[2].0, "agent.get");
        assert_eq!(recorded[2].1["target"], "w1:p1");
        assert_eq!(recorded[3].0, "pane.layout");
        assert_eq!(recorded[3].1["pane_id"], "w1:p1");
        drop(recorded);

        // A terminal task is immutable and must never trigger another read.
        let changed_again = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 102,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();
        assert!(!changed_again);
        assert_eq!(task.final_excerpt.as_deref(), Some(excerpt.as_str()));
        assert_eq!(requests.lock().unwrap().len(), 4);
        server.join().unwrap();
    }

    /// user parent | Given a child pane only four columns wide whose TUI agent
    /// hard-wrapped its report | When the terminal transition is reconciled |
    /// Then the task stays terminal and notifiable but publishes no
    /// `final_excerpt`, because column fragments are not an outcome the parent
    /// can close out on (#684).
    #[test]
    fn final_excerpt_recovers_logical_lines_from_a_narrow_pane() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let transcript = temp_socket().with_extension("jsonl");
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"older turn\"}]}}\n",
                "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n",
                "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"cb604c6f\"},{\"type\":\"text\",\"text\":\"T259-DONE\"}]}}\n",
            ),
        )
        .unwrap();
        let session_path = transcript.to_string_lossy().into_owned();
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": " cb\n 60\n 4c\n 6f\n\n T2\n 59\n -D\n ON\n E\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                layout_response(4),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done",
                    "state_change_seq": 2,
                    "agent_session": {"agent": "pi", "kind": "path", "value": session_path}}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-narrow-pane");

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        let excerpt = task.final_excerpt.clone().expect(
            "a four-column pane must still publish the agent's logical lines, not wrapping noise",
        );
        assert!(excerpt.contains("cb604c6f"), "excerpt: {excerpt:?}");
        assert!(excerpt.contains("T259-DONE"), "excerpt: {excerpt:?}");
        assert!(
            !excerpt.contains(" cb") && !excerpt.contains(" 60") && !excerpt.contains(" 4c"),
            "column fragments must never reach the parent: {excerpt:?}"
        );

        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded.len(),
            6,
            "read, verify, probe the width, bracket the transcript read with agent.get"
        );
        assert_eq!(recorded[3].0, "pane.layout");
        assert_eq!(recorded[4].0, "agent.get");
        assert_eq!(recorded[5].0, "agent.get");
        drop(recorded);
        server.join().unwrap();
        fs::remove_file(&socket).ok();
        fs::remove_file(&transcript).ok();
    }

    /// user parent | Given a narrow pane whose state sequence advances while the
    /// agent transcript is being read | When the transcript excerpt would be
    /// published | Then it is dropped, because the pane no longer serves this
    /// task's terminal transition.
    #[test]
    fn transcript_excerpt_is_dropped_when_the_pane_moves_during_the_read() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let transcript = temp_socket().with_extension("jsonl");
        fs::write(
            &transcript,
            "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"content\":\"cb604c6f\\nT259-DONE\"}}\n",
        )
        .unwrap();
        let session_path = transcript.to_string_lossy().into_owned();
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": " cb\n 60\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                layout_response(4),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done",
                    "state_change_seq": 2,
                    "agent_session": {"agent": "pi", "kind": "path", "value": session_path}}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 3}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-transcript-moved");

        apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert_eq!(
            task.final_excerpt, None,
            "a transcript read must not publish after the pane moved past the captured sequence"
        );
        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded.len(),
            6,
            "the fence must be re-proved after the read"
        );
        assert_eq!(recorded[5].0, "agent.get");
        drop(recorded);
        server.join().unwrap();
        fs::remove_file(&socket).ok();
        fs::remove_file(&transcript).ok();
    }

    /// user parent | Given a narrow pane whose agent transcript cannot be read |
    /// When the terminal transition is reconciled | Then the excerpt stays absent
    /// rather than falling back to wrapped viewport fragments.
    #[test]
    fn final_excerpt_stays_absent_for_a_narrow_pane_without_a_transcript() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": " cb\n 60\n 4c\n 6f\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                layout_response(4),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done",
                    "agent_session": {"agent": "pi", "kind": "path",
                                      "value": "/nonexistent/herdr-mcp/session.jsonl"}}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-narrow-no-transcript");

        apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(
            task.final_excerpt, None,
            "without a readable transcript a narrow pane must fail closed, never ship fragments"
        );
        server.join().unwrap();
        fs::remove_file(&socket).ok();
    }

    /// user parent | Given the pane width probe fails | When the terminal
    /// transition is reconciled | Then no excerpt is published, because unwrapped
    /// rows that cannot be qualified may be wrapping garbage.
    #[test]
    fn final_excerpt_stays_absent_when_the_width_probe_fails() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": " cb\n 60\n 4c\n 6f\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"error": {"code": "internal_error", "message": "layout unavailable"}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-width-probe-failed");

        apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert_eq!(
            task.final_excerpt, None,
            "an unverifiable pane width must fail closed instead of publishing unqualified rows"
        );
        server.join().unwrap();
        fs::remove_file(&socket).ok();
    }

    /// user parent | Given a child pane at the readable-width floor | When the
    /// terminal transition is reconciled | Then the excerpt is captured, so the
    /// narrow-pane guard never suppresses a usable outcome.
    #[test]
    fn final_excerpt_is_captured_at_the_readable_width_floor() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "cb604c6f\nT259-DONE\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                layout_response(crate::pane_width::MIN_READABLE_PANE_COLS),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-floor-pane");

        apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        let excerpt = task
            .final_excerpt
            .clone()
            .expect("a pane at the readable floor still publishes an excerpt");
        assert!(excerpt.contains("cb604c6f"));
        assert!(excerpt.contains("T259-DONE"));
        server.join().unwrap();
        fs::remove_file(&socket).ok();
    }

    /// user owner | Given a pane is reused before an old task is reconciled |
    /// When the event batch contains the old terminal event and newer activity |
    /// Then the old task stays terminal, never adopts the newer pane output, and
    /// the batch fence drops the capture before any native read.
    #[test]
    fn final_excerpt_is_omitted_when_the_pane_state_advanced() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![json!({"result": {"read": {"content": "OUTPUT FROM A DIFFERENT TASK"}}})],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-pane-reused");

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
                }),
                json!({
                    "cursor": 102,
                    "pane": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 3}
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_status.as_deref(), Some("done"));
        assert!(task.final_excerpt.is_none());
        assert!(
            requests.lock().unwrap().is_empty(),
            "a batch that already shows pane reuse must not read the reused pane"
        );
    }

    /// user owner | Given a live `pane.agent_status_changed` done event with no
    /// `state_change_seq`, while `agent.get` returns a stable done state | When
    /// the terminal transition is reconciled | Then the excerpt is omitted
    /// because no producer identity can pin the pane to this task, and the task
    /// stays terminal with the same lifecycle and cursor.
    #[test]
    fn final_excerpt_omits_live_event_without_state_change_seq_even_when_pre_and_post_agent_get_match()
     {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "LIVE EXCERPT OK\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                wide_layout_response(),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-shape");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane_id": "w1:p1",
                "agent": "codex",
                "agent_status": "done",
                "type": "pane.agent_status_changed"
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_status.as_deref(), Some("done"));
        assert_eq!(task.terminal_cursor, Some(101));
        assert_eq!(task.notification_state, "pending");
        assert!(
            task.final_excerpt.is_none(),
            "an unsequenced terminal event cannot be pinned to this task"
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "a transition without producer identity must not probe or read the pane"
        );
    }

    /// user owner | Given live events without `state_change_seq` where the pane
    /// is reused inside the same batch | When the terminal transition is
    /// reconciled | Then the batch fence keeps the task terminal with no
    /// excerpt and never reads the reused pane.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_later_same_pane_task_appears_in_batch() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![json!({"result": {"read": {"content": "OUTPUT FROM THE NEXT TASK"}}})],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-reused");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
                json!({
                    "cursor": 102,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "working",
                    "type": "pane.agent_status_changed"
                }),
                json!({
                    "cursor": 103,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(task.final_excerpt.is_none());
        assert!(
            requests.lock().unwrap().is_empty(),
            "a batch that already shows pane reuse must not read the reused pane"
        );
    }

    /// user owner | Given a sequenced terminal event | When the pane advances
    /// between the pre-read and post-read `agent.get` | Then the read output is
    /// dropped and no excerpt is stored.
    #[test]
    fn final_excerpt_omits_sequenced_event_when_seq_advances_between_pre_and_post_read() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "LATER TASK OUTPUT"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-advanced");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(
            task.final_excerpt.is_none(),
            "output read while the pane advanced must never be attributed to this task"
        );
        assert_eq!(requests.lock().unwrap().len(), 3);
        server.join().unwrap();
    }

    /// user owner | Given live events without `state_change_seq` where a second
    /// same-status `done` event follows the terminal one in the same batch |
    /// When the terminal transition is reconciled | Then the batch fence keeps
    /// the task terminal with no excerpt and issues no native read, because the
    /// later `done` can only be a second task that already finished.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_second_same_status_event_follows_in_batch() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![json!({"result": {"read": {"content": "SECOND TASK FINAL OUTPUT"}}})],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-repeat-done");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
                json!({
                    "cursor": 102,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(
            task.final_excerpt.is_none(),
            "a repeated live done event must not attribute the next task's output"
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "a batch that already shows a second completion must not read the pane"
        );
    }

    /// user owner | Given a live `pane.agent_status_changed` done event with no
    /// `state_change_seq`, followed in the same batch by a `pane_updated`
    /// snapshot of that same status and no sequence | When the terminal
    /// transition is reconciled | Then no excerpt is captured: the snapshot
    /// carries no identity either, so it cannot prove this task owns the pane.
    #[test]
    fn final_excerpt_omits_live_done_without_seq_when_same_status_pane_updated_follows() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "RC3_EXCERPT_UAT_PASS\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                wide_layout_response(),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-snapshot");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane_id": "w1:p1",
                    "agent": "pi",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
                json!({
                    "cursor": 104,
                    "type": "pane_updated",
                    "pane": {"pane_id": "w1:p1", "agent_status": "done"}
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(
            task.final_excerpt.is_none(),
            "an unsequenced snapshot must not unlock this task's excerpt"
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "the snapshot pair carries no identity, so the pane must not be read"
        );
    }

    /// user owner | Given task A's live `done` event without
    /// `state_change_seq`, a pane whose next task B already settled to the
    /// same `done` status with B's `working` event lost, and no reuse event
    /// in A's batch | When A's terminal transition is reconciled | Then A
    /// must keep no excerpt and the reused pane must never be read.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_pane_already_settled_to_a_later_same_status_task()
     {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        // Every response below belongs to the next task B: the capture must
        // never ask, or A adopts these values.
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
                json!({"result": {"read": {"content": "OUTPUT FROM THE NEXT TASK B"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-lost-working");
        // A was dispatched while the pane was still idle at sequence 1.
        task.baseline_seq = Some(1);

        // A's batch holds only A's live `done`: B's `working` was lost and
        // B's `done` has not reached this window, so no in-batch event can
        // prove the pane advanced.
        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane_id": "w1:p1",
                "agent": "codex",
                "agent_status": "done",
                "type": "pane.agent_status_changed"
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(
            task.final_excerpt.is_none(),
            "A must not adopt a later same-status task's output; got {:?}",
            task.final_excerpt
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "a pane that already settled to a later task must never be read"
        );
    }

    /// user owner | Given A's live `done` event followed by a same-status
    /// `pane_updated` snapshot, while the pane has actually already settled
    /// to a later task B | When A's terminal transition is reconciled | Then
    /// A must keep no excerpt, because the snapshot is not an identity anchor.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_same_status_snapshot_belongs_to_the_next_task()
     {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        // Every response below belongs to the next task B: the capture must
        // never ask, or A adopts these values.
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
                json!({"result": {"read": {"content": "OUTPUT FROM THE NEXT TASK B"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-lost-working-snapshot");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane_id": "w1:p1",
                    "agent": "codex",
                    "agent_status": "done",
                    "type": "pane.agent_status_changed"
                }),
                json!({
                    "cursor": 104,
                    "type": "pane_updated",
                    "pane": {"pane_id": "w1:p1", "agent_status": "done"}
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert!(
            task.final_excerpt.is_none(),
            "an unanchored same-status snapshot must not let A adopt B's output; got {:?}",
            task.final_excerpt
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "the reused pane must never be read on this shape"
        );
    }

    /// Contract check for the fix the RED cases above require: if the
    /// producer put the terminal transition's own sequence on the event, the
    /// existing consumer already rejects a pane that settled to a later task.
    ///
    /// user owner | Given a terminal event that carries its own
    /// `state_change_seq` and a pane already settled to a later task with a
    /// different sequence | When the transition is reconciled | Then the
    /// excerpt is omitted and the pane is never read.
    #[test]
    fn final_excerpt_omits_sequenced_terminal_event_when_the_pane_advanced_to_another_sequence() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![
                // The pane already reports the next task's settle.
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 4}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-sequenced-advanced");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {
                    "pane_id": "w1:p1",
                    "agent_status": "done",
                    "state_change_seq": 2
                }
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert!(
            task.final_excerpt.is_none(),
            "a terminal event pinned to sequence 2 must not adopt sequence 4 output"
        );
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1, "only the pre-read probe may run");
        assert_eq!(recorded[0].0, "agent.get");
        drop(recorded);
    }

    /// user owner | Given a sequenced terminal event whose pane state has not
    /// moved past the dispatch baseline | When the terminal transition is
    /// reconciled | Then no excerpt is captured and the pane is never read.
    #[test]
    fn final_excerpt_omits_sequenced_event_when_pre_read_seq_did_not_pass_baseline() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-stale-baseline");
        // The pane has not advanced since dispatch, so this sequence cannot
        // belong to this task's terminal transition.
        task.baseline_seq = Some(2);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert!(task.final_excerpt.is_none());
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1, "a stale pre-read sequence stops capture");
        assert_eq!(recorded[0].0, "agent.get");
        drop(recorded);
    }

    /// user owner | Given the same terminal transition observed twice with the
    /// same status and the same `state_change_seq` | When the batch is
    /// reconciled | Then the fence accepts it and still captures the excerpt.
    #[test]
    fn final_excerpt_captures_when_only_the_identical_transition_repeats_in_batch() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "COMPANION EVENT EXCERPT\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                wide_layout_response(),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-companion");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[
                json!({
                    "cursor": 101,
                    "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
                }),
                json!({
                    "cursor": 102,
                    "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
                }),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(
            task.final_excerpt.as_deref(),
            Some("COMPANION EVENT EXCERPT\n"),
            "an identical transition re-observation is not pane reuse"
        );
        server.join().unwrap();
    }

    /// user owner | Given terminal output longer than the byte cap | When the
    /// excerpt is shaped | Then at most 2 000 bytes are kept from the end on
    /// a UTF-8 boundary, prefixed with `…` and still ending on the last line.
    #[test]
    fn final_excerpt_is_byte_capped_from_the_end() {
        let filler = "背景 context line with multibyte text\n".repeat(80);
        let text = format!("{filler}\nFINAL-MARKER-LINE\n");
        let excerpt = shape_final_excerpt(&text);
        assert!(excerpt.len() <= FINAL_EXCERPT_MAX_BYTES);
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.trim_end().ends_with("FINAL-MARKER-LINE"));
    }

    /// user owner | Given a pane-closed terminal event | When the transition
    /// is reconciled | Then no excerpt is captured and no read is issued.
    #[test]
    fn pane_closed_tasks_get_no_final_excerpt() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![json!({"result": {"read": {"content": "must not be read"}}})],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);

        let mut task = excerpt_test_task("dispatch:prompt:excerpt-pane-closed");
        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({"cursor": 101, "pane_id": "w1:p1", "type": "pane.closed"})],
        )
        .unwrap();
        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Failed);
        assert_eq!(task.final_excerpt, None);
        assert!(
            requests.lock().unwrap().is_empty(),
            "pane closure must not trigger a native read"
        );
    }

    /// user owner | Given a task that times out awaiting activity | When the
    /// public task status is reconciled | Then it stays delivery_uncertain
    /// with no excerpt and no native read.
    #[test]
    fn delivery_uncertain_tasks_get_no_final_excerpt() {
        let _guard = crate::test_env::lock();
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![json!({"result": {"read": {"content": "must not be read"}}})],
            Arc::clone(&requests),
        );
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-excerpt-uncertain-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: None,
                client: Some(HerdrClient::new(&socket)),
            }),
        };
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-uncertain");
        task.lifecycle = PromptTaskLifecycle::AwaitingActivity;
        task.activity_observed = false;
        task.updated_at_ms = now_ms().saturating_sub(PROMPT_TASK_ACTIVITY_START_TIMEOUT_MS + 1_000);
        registry.save_task(&task, None).unwrap();

        let status = registry.task_status("dispatch:prompt:excerpt-uncertain");
        assert_eq!(status["ok"], true);
        assert_eq!(status["task"]["lifecycle"], "delivery_uncertain");
        assert!(
            status["task"].get("final_excerpt").is_none(),
            "uncertain tasks never expose an excerpt"
        );
        assert!(requests.lock().unwrap().is_empty());

        drop(registry);
        fs::remove_file(&db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }

    /// user owner | Given a native read that fails | When the terminal
    /// transition is reconciled | Then the task stays completed with no
    /// excerpt and lifecycle/notification state unchanged.
    #[test]
    fn final_excerpt_read_failure_keeps_lifecycle_and_notification() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let _server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"error": {"code": "agent_not_found", "message": "agent gone"}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);

        let mut task = excerpt_test_task("dispatch:prompt:excerpt-read-fails");
        let changed = apply_events_with_final_excerpt(
            Some(&client),
            None,
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        )
        .unwrap();
        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_status.as_deref(), Some("done"));
        assert_eq!(task.notification_state, "pending");
        assert_eq!(task.final_excerpt, None);
        assert_eq!(requests.lock().unwrap().len(), 2);

        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-excerpt-failure-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry::with_store(Arc::clone(&store));
        registry.save_task(&task, None).unwrap();
        let status = registry.task_status("dispatch:prompt:excerpt-read-fails");
        assert_eq!(status["task"]["lifecycle"], "completed");
        assert!(status["task"].get("final_excerpt").is_none());

        drop(registry);
        drop(store);
        fs::remove_file(&db_path).ok();
        fs::remove_file(db_path.with_extension("sqlite-wal")).ok();
        fs::remove_file(db_path.with_extension("sqlite-shm")).ok();
    }

    /// user owner | Given stored records with and without the excerpt field |
    /// When the ledger JSON is decoded | Then old rows still load, None is
    /// skipped on the wire, and the field round-trips.
    #[test]
    fn final_excerpt_field_is_rollback_safe_and_round_trips() {
        let base = excerpt_test_task("dispatch:prompt:excerpt-serde");
        assert_eq!(base.final_excerpt, None);
        let old_shape = serde_json::to_string(&base).unwrap();
        assert!(!old_shape.contains("final_excerpt"));
        let old_decoded: PromptTaskRecord = serde_json::from_str(&old_shape).unwrap();
        assert_eq!(old_decoded.final_excerpt, None);

        let mut upgraded = base.clone();
        upgraded.final_excerpt = Some("…final report line\n".to_owned());
        let encoded = serde_json::to_string(&upgraded).unwrap();
        assert!(encoded.contains("final_excerpt"));
        // Older runtimes tolerate the extra field: the record has no
        // deny_unknown_fields and unknown keys are ignored.
        let with_unknown = encoded.replace('}', ",\"future_field\": 42}");
        let decoded: PromptTaskRecord = serde_json::from_str(&with_unknown).unwrap();
        assert_eq!(decoded, upgraded);
    }

    /// user owner | Given multibyte child output longer than the budget | When
    /// it is shaped | Then the excerpt stays within the byte cap on a char
    /// boundary for every truncation offset.
    #[test]
    fn final_excerpt_cap_holds_for_multibyte_truncation() {
        for prefix in 0..4 {
            let text = format!("{}{}", "a".repeat(prefix), "界".repeat(1_000));
            let excerpt = shape_final_excerpt(&text);
            assert!(
                excerpt.len() <= FINAL_EXCERPT_MAX_BYTES,
                "{}",
                excerpt.len()
            );
            assert!(excerpt.starts_with('…'));
            assert!(excerpt.ends_with('界'));
        }
    }

    /// user owner | Given a terminal child with a captured excerpt | When the
    /// parent is woken | Then the wake text carries task facts only and never
    /// echoes child pane output into the parent's prompt.
    #[test]
    fn parent_wake_line_never_echoes_child_excerpt() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let wake_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let wake_capture = Arc::clone(&wake_texts);
        let server = thread::spawn(move || {
            let responses = vec![
                json!({"result": {"agent": {"pane_id": "w9:p9", "agent_status": "idle", "state_change_seq": 9}}}),
                json!({"result": {"ok": true}}),
            ];
            for mut response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                requests.lock().unwrap().push((
                    request["method"].as_str().unwrap_or_default().to_owned(),
                    request["params"].clone(),
                ));
                if request["method"] == "agent.prompt" {
                    wake_capture
                        .lock()
                        .unwrap()
                        .push(request["params"]["text"].as_str().unwrap().to_owned());
                }
                response["id"] = request["id"].clone();
                writeln!(stream, "{response}").unwrap();
            }
        });

        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-excerpt-wake-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let registry = PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: None,
                client: Some(HerdrClient::new(&socket)),
            }),
        };
        let mut task = excerpt_test_task("dispatch:prompt:tail-wake");
        task.lifecycle = PromptTaskLifecycle::Completed;
        task.terminal_status = Some("done".to_owned());
        task.terminal_cursor = Some(101);
        task.parent_target = Some("parent".to_owned());
        task.final_excerpt = Some(format!("working notes\n{}\n", "x".repeat(1_000)));
        registry.save_task(&task, None).unwrap();

        registry.maybe_notify_parent(&task);
        server.join().unwrap();

        let wake = wake_texts.lock().unwrap();
        assert_eq!(wake.len(), 1, "parent wake must be delivered once");
        let text = &wake[0];
        let line = text
            .lines()
            .find(|line| line.starts_with("- dispatch:prompt:tail-wake:"))
            .expect("wake lists the terminal task");
        assert!(line.ends_with("terminal_seq 101)"));
        assert!(!text.contains("working notes"));
        assert!(!text.contains("xxxx"));
    }

    fn task_agent_state(pane_id: &str, status: &str, seq: u64) -> AgentState {
        AgentState {
            pane_id: Some(pane_id.to_owned()),
            agent_status: Some(status.to_owned()),
            state_change_seq: Some(seq),
        }
    }

    /// Registers one running task through the real task path.
    fn register_running_task(
        registry: &PromptRegistry,
        task_id: &str,
        target: &str,
        pane_id: &str,
        parent_target: Option<&str>,
        parent_session_ref: Option<&str>,
        start_cursor: u64,
    ) -> PromptTaskRecord {
        let before = task_agent_state(pane_id, "idle", 1);
        let after = task_agent_state(pane_id, "working", 2);
        let task = registry
            .register_task(RegisterTaskInput {
                task_id,
                target,
                fingerprint: task_id,
                idempotency_key: None,
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor,
                parent_target,
                parent_session_ref,
                stall_after_ms: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Running);
        task
    }

    /// user planner | Given a session-scoped supervisor task that settles while
    /// its child still runs | When the child finishes before the supervisor
    /// settles again | Then the supervisor stays non-terminal with
    /// `awaiting_children` 1, the browser-session inbox never lists the child,
    /// and the supervisor's later, newer settle completes it.
    #[test]
    fn supervisor_task_holds_until_its_children_are_terminal() {
        let (db_path, registry) = task_registry("supervisor-hold");
        let supervisor_id = "dispatch:prompt:supervisor-hold";
        let child_id = "dispatch:prompt:supervisor-child";
        register_running_task(
            &registry,
            supervisor_id,
            "supervisor",
            "w1:p1",
            Some("web-planner"),
            Some("session:web"),
            100,
        );
        register_running_task(
            &registry,
            child_id,
            "worker-1",
            "w1:p2",
            Some("supervisor"),
            None,
            200,
        );

        // The supervisor's own turn settles while its child is still running.
        let mut settled = registry.load_task(supervisor_id).unwrap().unwrap();
        let held = registry
            .apply_task_window(
                &mut settled,
                101,
                &[json!({"cursor": 101, "pane_id": "w1:p1", "agent_status": "done"})],
            )
            .unwrap();
        assert!(held);
        assert_eq!(
            settled.last_cursor, 101,
            "a held settle still consumes its own event cursor"
        );
        assert_eq!(settled.lifecycle, PromptTaskLifecycle::Running);
        assert!(!settled.is_terminal());
        assert_eq!(settled.awaiting_children, Some(1));
        assert_eq!(settled.terminal_status, None);
        assert_eq!(settled.terminal_cursor, None);
        assert_eq!(settled.terminal_at_ms, None);
        assert!(
            settled.final_excerpt.is_none(),
            "a held settle must not capture an outcome excerpt"
        );
        registry.save_task(&settled, None).unwrap();

        let status = registry.task_status(supervisor_id);
        assert_eq!(status["task"]["lifecycle"], "running");
        assert_eq!(status["task"]["terminal"], false);
        assert_eq!(status["task"]["awaiting_children"], 1);
        assert!(status["task"]["terminal_cursor"].is_null());

        // The child finishes on its own lane. That alone must not settle the
        // supervisor: the hold is released by its own next settle.
        let mut child = registry.load_task(child_id).unwrap().unwrap();
        assert!(
            registry
                .apply_task_window(
                    &mut child,
                    201,
                    &[json!({"cursor": 201, "pane_id": "w1:p2", "agent_status": "done"})],
                )
                .unwrap()
        );
        assert_eq!(child.lifecycle, PromptTaskLifecycle::Completed);
        registry.save_task(&child, None).unwrap();
        assert_eq!(
            registry.task_status(supervisor_id)["task"]["awaiting_children"],
            1
        );

        // The browser session that dispatched the supervisor still sees no
        // terminal task, and never the supervisor's own child.
        let browser_held = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({}),
            Some("session:web"),
        );
        assert!(
            browser_held["tasks"].as_array().unwrap().is_empty(),
            "a held supervisor must not reach the browser-session inbox"
        );
        assert!(!browser_held.to_string().contains(child_id));
        // The supervisor's own inbox owns the finished child.
        let supervisor_inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_target": "supervisor"}),
            None,
        );
        assert_eq!(supervisor_inbox["tasks"][0]["task_id"], child_id);

        // The supervisor settles again after its children are terminal: the
        // strictly newer cursor is still applied and the hold clears.
        let mut resettled = registry.load_task(supervisor_id).unwrap().unwrap();
        assert_eq!(resettled.last_cursor, 101);
        assert!(
            registry
                .apply_task_window(
                    &mut resettled,
                    301,
                    &[json!({"cursor": 301, "pane_id": "w1:p1", "agent_status": "done"})],
                )
                .unwrap()
        );
        assert_eq!(resettled.lifecycle, PromptTaskLifecycle::Completed);
        assert!(resettled.is_terminal());
        assert_eq!(resettled.awaiting_children, None);
        assert_eq!(resettled.last_cursor, 301);
        registry.save_task(&resettled, None).unwrap();

        let done = registry.task_status(supervisor_id);
        assert_eq!(done["task"]["lifecycle"], "completed");
        assert_eq!(done["task"]["terminal"], true);
        assert!(done["task"].get("awaiting_children").is_none());

        let browser_done = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"view": "compact"}),
            Some("session:web"),
        );
        let listed = browser_done["tasks"].as_array().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["task_id"], supervisor_id);
        assert_eq!(browser_done["summary"]["completed"], 1);
        assert!(
            !browser_done.to_string().contains(child_id),
            "the supervisor's child is never session scoped"
        );

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user planner | Given a held supervisor task | When the compact inbox
    /// projection is applied | Then the `awaiting_children` fact survives the
    /// projection that session-scoped planners read.
    #[test]
    fn compact_projection_keeps_the_awaiting_children_fact() {
        let mut task = excerpt_test_task("dispatch:prompt:compact-hold");
        task.awaiting_children = Some(2);
        let projected = compact_task(task.public_json_at(now_ms()).as_object().unwrap().clone());
        assert_eq!(projected["awaiting_children"], 2);
        assert_eq!(projected["lifecycle"], "running");
    }

    /// user operator | Given a task record written before the hold fact existed
    /// | When the runtime decodes it | Then it loads with no hold and its
    /// public projection omits the field.
    #[test]
    fn legacy_task_records_load_without_the_awaiting_children_field() {
        let task = excerpt_test_task("dispatch:prompt:legacy-hold");
        let mut encoded = serde_json::to_value(&task).unwrap();
        assert!(
            encoded.get("awaiting_children").is_none(),
            "records without a hold must not carry the field on the wire"
        );
        encoded.as_object_mut().unwrap().remove("awaiting_children");
        let decoded: PromptTaskRecord = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.awaiting_children, None);
        assert!(
            decoded
                .public_json_at(now_ms())
                .get("awaiting_children")
                .is_none()
        );

        let mut held = task;
        held.awaiting_children = Some(3);
        let round_tripped: PromptTaskRecord =
            serde_json::from_value(serde_json::to_value(&held).unwrap()).unwrap();
        assert_eq!(round_tripped.awaiting_children, Some(3));
    }

    /// user planner | Given a dispatch whose parent_target resolves to the
    /// dispatcher's own pane | When it is submitted | Then it is rejected as
    /// `parent_target_is_target` before any prompt or ledger write.
    #[test]
    fn dispatch_rejects_a_parent_target_that_resolves_to_the_target_pane() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let prompt_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let idle = json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}}});
        let server = scripted_dispatch_server(
            listener,
            vec![idle.clone(), idle],
            Arc::clone(&prompt_texts),
        );
        let client = HerdrClient::new(&socket);
        let (db_path, registry) = task_registry("guard-self");

        let rejected = run_with_parent_session(
            &client,
            &registry,
            &json!({
                "target": "pi",
                "text": "delegate to myself",
                "parent_target": "w1:p1",
                "idempotency_key": "guard-self-1",
            }),
            None,
        );
        server.join().unwrap();

        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["code"], "parent_target_is_target");
        assert_eq!(rejected["retryable"], false);
        assert_eq!(rejected["task_id"], rejected["dispatch_id"]);
        assert!(
            prompt_texts.lock().unwrap().is_empty(),
            "a rejected dispatch must never prompt the agent"
        );
        assert!(
            registry.list_tasks(10).unwrap().is_empty(),
            "a rejected dispatch must not write a task row"
        );
        assert!(
            registry.inner.records.lock().unwrap().is_empty(),
            "a rejected dispatch must not reserve idempotency state"
        );

        drop(registry);
        remove_task_db(&db_path);
        fs::remove_file(socket).unwrap();
    }

    /// user planner | Given a worker task whose own parent is another active
    /// supervisor task | When the worker dispatches a sub-worker | Then it is
    /// rejected as `delegation_depth_exceeded` with nothing persisted, while the
    /// planner→supervisor→worker chain stays allowed.
    #[test]
    fn dispatch_rejects_delegation_below_a_worker() {
        let (db_path, registry) = task_registry("guard-depth");
        register_running_task(
            &registry,
            "dispatch:prompt:depth-supervisor",
            "supervisor",
            "w1:p1",
            Some("web-planner"),
            Some("session:web"),
            100,
        );
        register_running_task(
            &registry,
            "dispatch:prompt:depth-worker",
            "worker-1",
            "w1:p2",
            Some("supervisor"),
            None,
            200,
        );
        assert_eq!(registry.list_tasks(10).unwrap().len(), 2);

        let unreachable = HerdrClient::new(temp_socket());
        let rejected = run_with_parent_session(
            &unreachable,
            &registry,
            &json!({
                "target": "sub-worker",
                "text": "delegate deeper",
                "parent_target": "worker-1",
                "idempotency_key": "guard-depth-1",
            }),
            None,
        );
        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["code"], "delegation_depth_exceeded");
        assert_eq!(rejected["retryable"], false);
        assert_eq!(
            registry.list_tasks(10).unwrap().len(),
            2,
            "a rejected dispatch must not write a task row"
        );
        assert!(registry.inner.records.lock().unwrap().is_empty());

        // Web planner → supervisor → worker is the allowed depth: the planner is
        // a browser session, not another active task in the ledger.
        assert!(!delegation_depth_exceeded(&registry, "supervisor", Some("w1:p1")).unwrap());
        assert!(delegation_depth_exceeded(&registry, "worker-1", Some("w1:p2")).unwrap());

        // The allowed dispatch really runs end to end: the supervisor's own
        // parent link names no active task, so only the target and parent are
        // probed once each before the prompt.
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let prompt_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let worker = json!({"result": {"agent": {"pane_id": "w1:p3", "agent_status": "idle", "state_change_seq": 1}}});
        let supervisor = json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 3}}});
        let submitted = json!({"result": {"prompt": {"status": "submitted", "agent": {"pane_id": "w1:p3", "agent_status": "working", "state_change_seq": 2}}}});
        let server = scripted_dispatch_server(
            listener,
            vec![worker, supervisor, submitted],
            Arc::clone(&prompt_texts),
        );
        let client = HerdrClient::new(&socket);
        let allowed = run_with_parent_session(
            &client,
            &registry,
            &json!({
                "target": "worker-2",
                "text": "integrate the child output",
                "parent_target": "supervisor",
                "idempotency_key": "guard-depth-2",
            }),
            None,
        );
        server.join().unwrap();
        assert_eq!(allowed["ok"], true);
        assert_eq!(allowed["task"]["parent_target"], "supervisor");
        assert_eq!(allowed["task"]["lifecycle"], "running");
        assert_eq!(prompt_texts.lock().unwrap().len(), 1);
        assert_eq!(registry.list_tasks(10).unwrap().len(), 3);

        drop(registry);
        remove_task_db(&db_path);
        fs::remove_file(socket).unwrap();
    }

    /// user planner | Given a session-scoped supervisor task that blocks while
    /// its child still runs | When the block window is applied | Then it
    /// surfaces as terminal `blocked` immediately and keeps the outstanding
    /// child count as a fact, so the parent is woken instead of deadlocking.
    #[test]
    fn blocked_supervisor_surfaces_immediately_with_awaiting_children() {
        let (db_path, registry) = task_registry("supervisor-blocked");
        let supervisor_id = "dispatch:prompt:supervisor-blocked";
        let child_id = "dispatch:prompt:supervisor-blocked-child";
        register_running_task(
            &registry,
            supervisor_id,
            "supervisor",
            "w1:p1",
            Some("web-planner"),
            Some("session:web"),
            100,
        );
        register_running_task(
            &registry,
            child_id,
            "worker-1",
            "w1:p2",
            Some("supervisor"),
            None,
            200,
        );

        let mut blocked = registry.load_task(supervisor_id).unwrap().unwrap();
        assert!(
            registry
                .apply_task_window(
                    &mut blocked,
                    101,
                    &[json!({"cursor": 101, "pane_id": "w1:p1", "agent_status": "blocked"})],
                )
                .unwrap()
        );
        assert_eq!(blocked.lifecycle, PromptTaskLifecycle::Blocked);
        assert!(blocked.is_terminal());
        assert_eq!(blocked.terminal_status.as_deref(), Some("blocked"));
        assert_eq!(blocked.terminal_cursor, Some(101));
        assert_eq!(blocked.awaiting_children, Some(1));
        registry.save_task(&blocked, None).unwrap();

        let status = registry.task_status(supervisor_id);
        assert_eq!(status["task"]["lifecycle"], "blocked");
        assert_eq!(status["task"]["terminal"], true);
        assert_eq!(status["task"]["awaiting_children"], 1);

        // A blocked supervisor is never held: the browser inbox is woken at
        // once, while the running child stays out of it.
        let browser = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({}),
            Some("session:web"),
        );
        let listed = browser["tasks"].as_array().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["task_id"], supervisor_id);
        assert_eq!(listed[0]["lifecycle"], "blocked");
        assert_eq!(listed[0]["awaiting_children"], 1);
        assert!(!browser.to_string().contains(child_id));

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user extension | Given a session whose supervisor task is still running |
    /// When the extension inbox is read with the opt-in running view | Then it
    /// gets one bounded compact row for that session only, with no excerpt and
    /// no ack, and the default inbox keeps listing terminal tasks only.
    #[test]
    fn extension_inbox_can_include_running_task_rows() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let semantic_config_dir = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-extension-semantic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &semantic_config_dir);
        }
        let db_path = env::temp_dir().join(format!(
            "herdr-mcp-agent-task-extension-running-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(Mutex::new(StateStore::open(&db_path).unwrap()));
        let cache = Arc::new(EventCache::from_snapshot_for_test(json!({
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1"},
                {"pane_id": "w1:p3", "workspace_id": "w1"},
                {"pane_id": "w2:p9", "workspace_id": "w2"},
            ],
            "agents": [],
            "workspaces": [{"workspace_id": "w1"}, {"workspace_id": "w2"}],
        })));
        let registry = PromptRegistry {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                #[cfg(test)]
                clock: None,
                records: Mutex::new(HashMap::new()),
                handoff_injected_panes: Mutex::new(HashSet::new()),
                store: Some(store),
                cache: Some(cache),
                client: None,
            }),
        };
        let supervisor_id = "dispatch:prompt:extension-running";
        let other_id = "dispatch:prompt:extension-running-other";
        register_running_task(
            &registry,
            supervisor_id,
            "supervisor",
            "w1:p1",
            None,
            Some("session:web"),
            100,
        );
        register_running_task(
            &registry,
            "dispatch:prompt:extension-running-child",
            "worker-1",
            "w1:p3",
            Some("supervisor"),
            None,
            100,
        );
        register_running_task(
            &registry,
            other_id,
            "supervisor",
            "w2:p9",
            None,
            Some("session:other"),
            100,
        );

        // Default projection is unchanged: no terminal task, no rows.
        let default_view = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"limit": 8}),
            Some("session:web"),
        );
        assert!(default_view["tasks"].as_array().unwrap().is_empty());

        let included = registry.task_inbox_with_running(None, "session:web", 8);
        let rows = included["tasks"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["task_id"], supervisor_id);
        assert_eq!(rows[0]["workspace_id"], "w1");
        assert_eq!(rows[0]["lifecycle"], "running");
        assert_eq!(rows[0]["terminal"], false);
        assert!(rows[0].get("final_excerpt").is_none());
        assert!(rows[0].get("acknowledged").is_none());
        assert!(rows[0].get("awaiting_children").is_none());
        assert_eq!(included["summary"]["running"], 1);
        assert!(
            !included.to_string().contains(other_id),
            "another session's running task stays out of scope"
        );

        // The caller's own workspace and limit still bound the projection.
        let other_workspace = registry.task_inbox_with_running(Some("w2"), "session:web", 8);
        assert!(other_workspace["tasks"].as_array().unwrap().is_empty());
        let bounded = registry.task_inbox_with_running(None, "session:web", 1);
        assert_eq!(bounded["tasks"].as_array().unwrap().len(), 1);

        // A held supervisor keeps exposing its hold fact in the same row.
        let mut held = registry.load_task(supervisor_id).unwrap().unwrap();
        assert!(
            registry
                .apply_task_window(
                    &mut held,
                    101,
                    &[json!({"cursor": 101, "pane_id": "w1:p1", "agent_status": "done"})],
                )
                .unwrap()
        );
        registry.save_task(&held, None).unwrap();
        let held_view = registry.task_inbox_with_running(None, "session:web", 8);
        let held_rows = held_view["tasks"].as_array().unwrap();
        assert_eq!(held_rows.len(), 1);
        assert_eq!(held_rows[0]["lifecycle"], "running");
        assert_eq!(held_rows[0]["awaiting_children"], 1);

        drop(registry);
        remove_task_db(&db_path);
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        fs::remove_dir_all(&semantic_config_dir).ok();
    }
}
