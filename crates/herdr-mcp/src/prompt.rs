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
use std::collections::HashMap;
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
const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;
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
    parent_target: Option<String>,
    #[serde(default)]
    parent_session_ref: Option<String>,
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
        json!({
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
        })
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

#[derive(Default)]
struct PromptRegistryInner {
    records: Mutex<HashMap<String, PromptRecord>>,
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
            parent_target: parent_target.map(str::to_owned),
            parent_session_ref: parent_session_ref.map(str::to_owned),
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
        self.reconcile_active_tasks();
        match self.list_tasks(512) {
            Ok(tasks) => {
                let scoped = tasks
                    .iter()
                    .filter(|task| {
                        task_matches_scope(task, workspace_id, parent_target, parent_session_ref)
                    })
                    .filter(|task| {
                        !task.is_terminal()
                            || include_acknowledged
                            || task.acknowledged_at_ms.is_none()
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

                json!({
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
                })
            }
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
            }),
        }
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
        let digest = cache.digest_since(task.last_cursor);
        if digest.cursor.saturating_sub(task.last_cursor) > 2048 {
            changed |= mark_delivery_uncertain(&mut task, now_ms(), "event_history_gap");
        } else {
            for event in &digest.events {
                if apply_task_event(&mut task, event) {
                    changed = true;
                }
            }
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
            idempotency_key: None,
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

fn apply_task_event(task: &mut PromptTaskRecord, event: &Value) -> bool {
    let cursor = event.get("cursor").and_then(Value::as_u64).unwrap_or(0);
    if cursor <= task.start_cursor || cursor <= task.last_cursor {
        return false;
    }
    let pane_id = event.get("pane_id").and_then(Value::as_str).or_else(|| {
        event
            .get("pane")
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
    });
    if task.pane_id.as_deref() != pane_id {
        return false;
    }

    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .replace('.', "_");
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
                "hint": "The prompt was not submitted because durable state could not be recorded. A new submission requires a healthy local state store.",
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
            i64::try_from(expires_at).unwrap_or(i64::MAX),
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
            registry.task_inbox(
                workspace_id,
                parent_target,
                parent_session_ref,
                include_acknowledged,
                advisory,
                limit,
            )
        }
        AGENT_TASK_ACK_METHOD => {
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
    // Native Herdr 0.9.x wait is lifecycle/status based and cannot identify the
    // prompt turn. Submit exactly once without native wait, then wait on the
    // Runtime-owned task ledger below.
    let params = json!({
        "target": target,
        "text": text,
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

    let submitted = result.get("submitted").and_then(Value::as_bool);
    let after = agent_state_from_result(&result);
    match registry.register_task(RegisterTaskInput {
        task_id,
        target,
        fingerprint: &fingerprint,
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
}
