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
}

impl PromptTaskRecord {
    fn is_terminal(&self) -> bool {
        self.lifecycle.is_terminal()
    }

    fn public_json(&self) -> Value {
        let mut public = json!({
            "task_id": self.task_id,
            "dispatch_id": self.dispatch_id,
            "target": self.target,
            "pane_id": self.pane_id,
            "workspace_id": self.workspace_id,
            "lifecycle": self.lifecycle.as_str(),
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
        if let Some(excerpt) = &self.final_excerpt
            && let Some(object) = public.as_object_mut()
        {
            object.insert("final_excerpt".to_owned(), json!(excerpt));
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
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<Mutex<StateStore>>) -> Self {
        Self {
            inner: Arc::new(PromptRegistryInner {
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
        } = input;
        if submitted == Some(false) || self.inner.store.is_none() {
            return Ok(None);
        }
        if let Some(existing) = self.load_task(task_id)? {
            return Ok(Some(existing));
        }

        let now = now_ms();
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
            final_excerpt: None,
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
        self.save_task(&record, Some(fingerprint))?;
        let reconciled = self.reconcile_task(task_id)?;
        Ok(reconciled.or(Some(record)))
    }

    pub fn task_status(&self, task_id: &str) -> Value {
        match self.reconcile_task(task_id) {
            Ok(Some(task)) => json!({"ok": true, "task": task.public_json()}),
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
                json!({"ok": true, "task": task.public_json()})
            }
            Ok(_) => json!({"ok": false, "code": "agent_task_not_found"}),
            Err(error) => json!({
                "ok": false, "code": "agent_task_store_unavailable", "message": error,
            }),
        }
    }

    pub fn task_status_for_parent_session(&self, task_id: &str, parent_session_ref: &str) -> Value {
        match self.reconcile_task(task_id) {
            Ok(Some(task)) if task.parent_session_ref.as_deref() == Some(parent_session_ref) => {
                json!({"ok": true, "task": task.public_json()})
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
        let escalation = task_escalation(tasks.iter().filter(|task| {
            task_matches_scope(task, workspace_id, parent_target, parent_session_ref)
        }));

        Ok(json!({
            "ok": true,
            "summary": {
                "running": running,
                "completed": completed,
                "blocked": blocked,
                "failed": failed,
                "uncertain": uncertain,
            },
            "tasks": terminal.into_iter().map(|task| task.public_json()).collect::<Vec<_>>(),
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
        match self.load_task(task_id) {
            Ok(Some(mut task)) => {
                let now = now_ms();
                task.acknowledged_at_ms = Some(now);
                task.notification_state = "acknowledged".to_owned();
                task.updated_at_ms = now;
                match self.save_task(&task, None) {
                    Ok(()) => json!({"ok": true, "task": task.public_json()}),
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
                            "task": task.public_json(),
                        });
                    }
                    if task.is_terminal() {
                        return json!({
                            "completed": true,
                            "task_id": task_id,
                            "matched": task.lifecycle.as_str(),
                            "task": task.public_json(),
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
            if std::time::Instant::now() >= deadline {
                let task = self
                    .reconcile_task(task_id)
                    .ok()
                    .flatten()
                    .map(|task| task.public_json())
                    .unwrap_or(Value::Null);
                return json!({
                    "completed": false,
                    "task_id": task_id,
                    "reason": "agent_task_wait_timeout",
                    "task": task,
                });
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

        let now = now_ms();
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
        let Some(mut task) = self.load_task(task_id)? else {
            return Ok(None);
        };
        if task.is_terminal() {
            return Ok(Some(task));
        }
        let mut changed = false;
        if task.lifecycle == PromptTaskLifecycle::AwaitingActivity
            && now_ms().saturating_sub(task.updated_at_ms) >= PROMPT_TASK_ACTIVITY_START_TIMEOUT_MS
        {
            changed |= mark_delivery_uncertain(&mut task, now_ms(), "activity_start_timeout");
        }
        if task.is_terminal() {
            if changed {
                self.save_task(&task, None)?;
                self.maybe_notify_parent(&task);
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
            changed |= mark_delivery_uncertain(&mut task, now_ms(), reason);
            if changed {
                self.save_task(&task, None)?;
                self.maybe_notify_parent(&task);
                if let Some(reloaded) = self.load_task(task_id)? {
                    task = reloaded;
                }
            }
            return Ok(Some(task));
        }
        let digest = cache.digest_since(task.last_cursor);
        if digest.cursor.saturating_sub(task.last_cursor) > 2048 {
            changed |= mark_delivery_uncertain(&mut task, now_ms(), "event_history_gap");
        } else {
            changed |= apply_events_with_final_excerpt(
                self.inner.client.as_ref(),
                &mut task,
                &digest.events,
            );
        }
        if digest.cursor > task.last_cursor {
            task.last_cursor = digest.cursor;
            changed = true;
        }
        if changed {
            self.save_task(&task, None)?;
            if task.is_terminal() {
                self.maybe_notify_parent(&task);
                if let Some(reloaded) = self.load_task(task_id)? {
                    task = reloaded;
                }
            }
        }
        Ok(Some(task))
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
/// `agent.read` round trip.
const COMPACT_TASK_FIELDS: [&str; 9] = [
    "task_id",
    "target",
    "pane_id",
    "lifecycle",
    "terminal",
    "acknowledged",
    "terminal_at_ms",
    "updated_at_ms",
    "final_excerpt",
];

fn compact_task(task: Map<String, Value>) -> Map<String, Value> {
    task.into_iter()
        .filter(|(key, _)| {
            COMPACT_TASK_FIELDS.contains(&key.as_str()) || key.starts_with("attention")
        })
        .collect()
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
fn task_escalation<'a>(tasks: impl IntoIterator<Item = &'a PromptTaskRecord>) -> Value {
    let now = now_ms();
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

        for task in lane.iter().copied().filter(|task| {
            matches!(
                task.lifecycle,
                PromptTaskLifecycle::Running | PromptTaskLifecycle::AwaitingActivity
            )
        }) {
            let idle_ms = now.saturating_sub(task.updated_at_ms);
            if idle_ms < ESCALATION_STALL_IDLE_MS {
                continue;
            }
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

/// Applies cache events to a durable task record and, exactly once at an
/// eligible terminal transition, captures the bounded final excerpt of the
/// child's recent output. A failed capture only leaves `final_excerpt`
/// unset; lifecycle, notification, and ack state are untouched.
fn apply_events_with_final_excerpt(
    client: Option<&HerdrClient>,
    task: &mut PromptTaskRecord,
    events: &[Value],
) -> bool {
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
    if changed
        && task.is_terminal()
        && task.final_excerpt.is_none()
        && final_excerpt_eligible(task)
        && !batch_proves_pane_advanced(task, events, terminal_state_change_seq)
    {
        task.final_excerpt = read_final_excerpt(client, task, terminal_state_change_seq);
    }
    changed
}

/// Returns true when this same event batch already proves the pane moved past
/// the task's terminal cursor, i.e. the pane may be serving a later task. Live
/// Herdr events carry no `state_change_seq`, so a later same-pane status change
/// or pane closure is by itself enough to fail the capture closed before any
/// native read; a differing sequence does the same when events carry one. A
/// `pane_updated` snapshot of the same terminal status is the companion of
/// `pane.agent_status_changed` and does not prove a later task.
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
/// read. Live Herdr terminal events carry no sequence, in which case the pre-read
/// `agent.get` supplies it (and must prove the pane moved past the dispatch
/// baseline).
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
    Some(excerpt)
}

/// Returns the pane state sequence this capture is fenced on, after proving the
/// pane still reports this task's terminal status. An event that carries a
/// sequence pins it directly; a live event without one is pinned by the
/// pre-read `agent.get`, which must also have advanced past the baseline.
fn final_excerpt_capture_seq(
    client: &HerdrClient,
    task: &PromptTaskRecord,
    target: &str,
    expected_state_change_seq: Option<u64>,
) -> Option<u64> {
    let pre = final_excerpt_agent_state(client, target)?;
    if pre.pane_id.as_deref() != Some(target)
        || pre.agent_status.as_deref() != task.terminal_status.as_deref()
    {
        return None;
    }
    let pre_seq = pre.state_change_seq?;
    if expected_state_change_seq.is_some_and(|expected_seq| expected_seq != pre_seq) {
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
    let before = agent_state_of(client, target);
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
    let dispatch_text = injection
        .as_ref()
        .map_or_else(|| text.to_owned(), |injection| injection.text.clone());
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
    }) {
        Ok(Some(task)) => {
            if let Some(object) = result.as_object_mut() {
                object.insert("task_tracking".to_owned(), json!(true));
                object.insert("turn_id".to_owned(), json!(task.turn_id));
                object.insert("task".to_owned(), task.public_json());
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
        if !completed {
            object.insert("ok".to_owned(), json!(false));
            object.insert("failure".to_owned(), json!("agent_task_wait_timeout"));
            object.insert(
                "failure_phase".to_owned(),
                json!("post_submission_task_wait"),
            );
            object.insert("retryable".to_owned(), json!(false));
            object.insert(
                "hint".to_owned(),
                json!("the prompt was submitted once; task wait timed out, so inspect the returned task_id/inbox before any retry"),
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
            final_excerpt: None,
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
                "final_excerpt",
                "lifecycle",
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
            final_excerpt: None,
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
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);

        let mut task = excerpt_test_task("dispatch:prompt:excerpt-once");
        let changed = apply_events_with_final_excerpt(
            Some(&client),
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        );
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
        assert_eq!(recorded.len(), 3, "capture must verify, read and re-verify");
        assert_eq!(recorded[0].0, "agent.get");
        assert_eq!(recorded[0].1["target"], "w1:p1");
        assert_eq!(recorded[1].0, "agent.read");
        assert_eq!(recorded[1].1["target"], "w1:p1");
        assert_eq!(recorded[1].1["source"], "recent_unwrapped");
        assert_eq!(recorded[1].1["lines"], FINAL_EXCERPT_LINES);
        assert_eq!(recorded[1].1["strip_ansi"], true);
        assert_eq!(recorded[2].0, "agent.get");
        assert_eq!(recorded[2].1["target"], "w1:p1");
        drop(recorded);

        // A terminal task is immutable and must never trigger another read.
        let changed_again = apply_events_with_final_excerpt(
            Some(&client),
            &mut task,
            &[json!({
                "cursor": 102,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        );
        assert!(!changed_again);
        assert_eq!(task.final_excerpt.as_deref(), Some(excerpt.as_str()));
        assert_eq!(requests.lock().unwrap().len(), 3);
        server.join().unwrap();
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
        );

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_status.as_deref(), Some("done"));
        assert!(task.final_excerpt.is_none());
        assert!(
            requests.lock().unwrap().is_empty(),
            "a batch that already shows pane reuse must not read the reused pane"
        );
    }

    /// user owner | Given live Herdr events that carry no `state_change_seq` |
    /// When the pre-read and post-read `agent.get` agree on the pane state |
    /// Then the excerpt is captured from the pane state observed before the read.
    #[test]
    fn final_excerpt_captures_live_event_without_state_change_seq_when_pre_and_post_agent_get_match()
     {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "LIVE EXCERPT OK\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-shape");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
            &mut task,
            &[json!({
                "cursor": 101,
                "pane_id": "w1:p1",
                "agent": "codex",
                "agent_status": "done",
                "type": "pane.agent_status_changed"
            })],
        );

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert_eq!(
            task.final_excerpt.as_deref(),
            Some("LIVE EXCERPT OK\n"),
            "the live event shape must still capture a fenced excerpt"
        );
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].0, "agent.get");
        assert_eq!(recorded[1].0, "agent.read");
        assert_eq!(recorded[2].0, "agent.get");
        drop(recorded);
        server.join().unwrap();
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
        );

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert!(task.final_excerpt.is_none());
        assert!(
            requests.lock().unwrap().is_empty(),
            "a batch that already shows pane reuse must not read the reused pane"
        );
    }

    /// user owner | Given a live event without `state_change_seq` | When the
    /// pane advances between the pre-read and post-read `agent.get` | Then the
    /// read output is dropped and no excerpt is stored.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_seq_advances_between_pre_and_post_read() {
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
            &mut task,
            &[json!({
                "cursor": 101,
                "pane_id": "w1:p1",
                "agent": "codex",
                "agent_status": "done",
                "type": "pane.agent_status_changed"
            })],
        );

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
        );

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

    /// user owner | Given a live `pane.agent_status_changed` done event followed
    /// in the same batch by a `pane_updated` snapshot of that same status and
    /// no sequence | When the terminal transition is reconciled | Then the
    /// snapshot is not a later task and the fenced excerpt is captured.
    #[test]
    fn final_excerpt_captures_live_done_when_same_status_pane_updated_follows_without_seq() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let server = scripted_read_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
                json!({"result": {"read": {"content": "RC3_EXCERPT_UAT_PASS\n"}}}),
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}}}),
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-live-snapshot");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
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
        );

        assert!(changed);
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Completed);
        assert_eq!(task.terminal_cursor, Some(101));
        assert_eq!(
            task.final_excerpt.as_deref(),
            Some("RC3_EXCERPT_UAT_PASS\n"),
            "a same-status pane_updated snapshot must not hide this task's excerpt"
        );
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].0, "agent.get");
        assert_eq!(recorded[1].0, "agent.read");
        assert_eq!(recorded[2].0, "agent.get");
        drop(recorded);
        server.join().unwrap();
    }

    /// user owner | Given a live event without `state_change_seq` whose pane
    /// state has not moved past the dispatch baseline | When the terminal
    /// transition is reconciled | Then no excerpt is captured and the pane is
    /// never read.
    #[test]
    fn final_excerpt_omits_live_event_without_seq_when_pre_read_seq_did_not_pass_baseline() {
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
            &mut task,
            &[json!({
                "cursor": 101,
                "pane_id": "w1:p1",
                "agent": "codex",
                "agent_status": "done",
                "type": "pane.agent_status_changed"
            })],
        );

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
            ],
            Arc::clone(&requests),
        );
        let client = HerdrClient::new(&socket);
        let mut task = excerpt_test_task("dispatch:prompt:excerpt-companion");
        task.baseline_seq = Some(1);

        let changed = apply_events_with_final_excerpt(
            Some(&client),
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
        );

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
            &mut task,
            &[json!({"cursor": 101, "pane_id": "w1:p1", "type": "pane.closed"})],
        );
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
            &mut task,
            &[json!({
                "cursor": 101,
                "pane": {"pane_id": "w1:p1", "agent_status": "done", "state_change_seq": 2}
            })],
        );
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
}
