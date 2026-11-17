pub(crate) mod decision_gates;
mod role_targets;

use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::progressive_skills::agent_attention_advice;
use crate::state_cache::EventCache;
use crate::state_store::decision_gates::DecisionGate;
use crate::state_store::{
    OperationLedgerInput, OperationLedgerRecord, OperationReservation, StateStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECORD_TTL_MS: u64 = 10 * 60_000;
const RECORD_LIMIT: usize = 512;
const PROMPT_OPERATION_KIND: &str = "herdr_prompt";
const PROMPT_TASK_OPERATION_KIND: &str = "herdr_prompt_task";
const PROMPT_TASK_TTL_MS: u64 = 7 * 24 * 60 * 60_000;
/// S4: repair redispatches allowed for one receipt. Caller-identified via
/// `repair_of`; the budget is per receipt, not per lane, and exhaustion is a
/// terminal repair state (never `delivery_uncertain`).
pub(crate) const REPAIR_BUDGET_ATTEMPTS: u32 = 3;
/// S4: retention is configurable but bounded. Terminal task rows past this
/// window leave default inbox queries and stay in the archive; they are never
/// deleted by the runtime.
pub(crate) const PROMPT_TASK_RETENTION_ENV: &str = "HERDR_MCP_AGENT_TASK_RETENTION_MS";
const PROMPT_TASK_RETENTION_MIN_MS: u64 = 60 * 60_000;
const PROMPT_TASK_RETENTION_MAX_MS: u64 = 30 * 24 * 60 * 60_000;

fn prompt_task_ttl_ms() -> u64 {
    parse_retention_ms(std::env::var(PROMPT_TASK_RETENTION_ENV).ok().as_deref())
}

/// Pure retention parse: unset/invalid keeps the default, every configured
/// value is clamped into the supported window.
fn parse_retention_ms(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(PROMPT_TASK_RETENTION_MIN_MS, PROMPT_TASK_RETENTION_MAX_MS))
        .unwrap_or(PROMPT_TASK_TTL_MS)
}
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
/// #332 B-2: bound the foreground blocker observation so a hung probe never
/// delays task registration. Names are process names, never raw argv.
const FOREGROUND_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const FOREGROUND_BLOCKER_MAX_PROCESSES: usize = 8;
const FOREGROUND_NAME_MAX_CHARS: usize = 64;
const FOREGROUND_COMMAND_MAX_TOKENS: usize = 6;
const FOREGROUND_COMMAND_MAX_CHARS: usize = 120;
static NEXT_DISPATCH_NONCE: AtomicU64 = AtomicU64::new(1);

/// One `spawn` call must fit the whole chain (pane create, `agent.start`,
/// readiness, prompt submission) inside the budget the caller actually waits.
/// A WebChat `herdr_call` without `timeout_ms` is cut by Edge at 30s and the
/// link wire never exceeds 60s, so the default keeps headroom under the
/// smallest transport instead of reserving work the caller will never see.
pub(crate) const SPAWN_CHAIN_DEFAULT_MS: u64 = 25_000;
/// Accepted ceiling for `spawn.startup_timeout_ms`. The local CLI waits the
/// chain plus slack and never exceeds its own 120s transport cap.
pub(crate) const SPAWN_CHAIN_MAX_MS: u64 = 100_000;
/// Headroom kept inside the chain for the prompt submission itself. A quarter
/// of the chain keeps tiny budgets workable instead of reserving more than the
/// caller granted.
const SPAWN_PROMPT_RESERVE_MS: u64 = 5_000;
/// Native `agent.start` rejects `timeout_ms` at or below its 3s floor.
const SPAWN_NATIVE_START_FLOOR_MS: u64 = 3_001;
const SPAWN_READY_POLL: Duration = Duration::from_millis(200);
/// A readiness window shorter than one poll cannot prove readiness, so the
/// chain reports a resumable checkpoint instead of an orphan in-flight key.
const SPAWN_READY_MIN_POLL_MS: u64 = 250;
const SPAWN_READY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 1.2 S5: how long after a confirmed `agent.start` the chain keeps watching
/// for an early exit. A healthy agent is held for the whole window: presence
/// does not prove liveness, and dead-on-start agents often die several
/// seconds in. The window is capped by the chain budget and prompt reserve
/// so a small caller budget still keeps its prompt reserve.
const SPAWN_EARLY_EXIT_WINDOW_MS: u64 = 10_000;
/// Poll cadence inside the early-exit window; coarser than readiness polling
/// because death is a discrete event, not a latency-sensitive transition.
const SPAWN_EARLY_EXIT_POLL: Duration = Duration::from_millis(500);
/// Bound on the sanitized pane tail attached to an `agent_start_failed`.
const SPAWN_FAILURE_TAIL_LINES: usize = 20;
/// S4 ready criterion, part 2: a self-reported `agent_status == "idle"` counts
/// as ready only after it has been observed continuously for this settle
/// window. `interactive_ready == true` is the agent's own handshake and needs
/// no settle. The window is a floor on the heuristic, never an added delay on
/// the handshake path.
const SPAWN_READY_SETTLE_MS: u64 = 250;
/// Upper bound for one pane mutation, always cut down to the remaining chain.
const SPAWN_PANE_TIMEOUT_MS: u64 = 10_000;
/// Lower bound for one pane socket call. A caller may grant the chain less
/// than this; the pane step still gets a usable window because the chain
/// deadline is enforced by the `remaining == 0` gates around each step, not by
/// starving a single in-flight mutation of any socket time at all.
const SPAWN_PANE_SOCKET_FLOOR_MS: u64 = 4_000;
const SPAWN_AGENT_NAME_MAX: usize = 32;

/// #332 B-1: maps the agent-turn `lifecycle` onto the prompt steering
/// vocabulary used by `steering_state`. `DeliveryUncertain` degrades to
/// `submitted`: the runtime accepted the submission but cannot prove the
/// agent saw it.
fn steering_for_lifecycle(lifecycle: &PromptTaskLifecycle) -> &'static str {
    match lifecycle {
        PromptTaskLifecycle::AwaitingPriorSettle => "queued",
        PromptTaskLifecycle::AwaitingActivity => "delivered",
        PromptTaskLifecycle::Running => "acknowledged",
        PromptTaskLifecycle::Completed => "applied",
        PromptTaskLifecycle::Blocked | PromptTaskLifecycle::Failed => "rejected",
        PromptTaskLifecycle::DeliveryUncertain => "submitted",
    }
}

/// Advances the persisted steering stage to match a lifecycle the record just
/// entered. A record marked `superseded` stays superseded until its own
/// outcome lands (the agent picked it up anyway, or it went terminal);
/// in-between transitions like prior-settle must never erase the fact.
fn update_steering(task: &mut PromptTaskRecord) {
    let mapped = steering_for_lifecycle(&task.lifecycle);
    if task.steering_state.as_deref() == Some("superseded")
        && matches!(mapped, "queued" | "delivered" | "submitted")
    {
        return;
    }
    task.steering_state = Some(mapped.to_owned());
    // #332 B-2: a foreground blocker is only observable while the steering is
    // still unacknowledged; an own outcome (acknowledged/applied/rejected)
    // clears it so status never claims a finished prompt is still blocked.
    if matches!(mapped, "acknowledged" | "applied" | "rejected") {
        task.foreground_blocker = None;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct AgentState {
    pane_id: Option<String>,
    agent_status: Option<String>,
    state_change_seq: Option<u64>,
    /// Agent kind (e.g. `pi`, `codex`) when the reporting surface provides
    /// one. #332 B-2 uses it to keep the agent's own foreground process out
    /// of the blocker summary. None on older/partial observations.
    kind: Option<String>,
}

struct RegisterTaskInput<'a> {
    task_id: &'a str,
    target: &'a str,
    requested_target: Option<&'a str>,
    fingerprint: &'a str,
    idempotency_key: Option<&'a str>,
    before: Option<&'a AgentState>,
    after: Option<&'a AgentState>,
    submitted: Option<bool>,
    start_cursor: u64,
    parent_target: Option<&'a str>,
    parent_session_ref: Option<&'a str>,
    stall_after_ms: Option<u64>,
    max_runtime_ms: Option<u64>,
    on_budget: Option<&'a str>,
    /// S0 wire correlation id (`RuntimeContext.wire_request_id`) for the call
    /// that created this task. Persisted so the terminal inbox row can be
    /// matched back to the envelope `request_id` the dispatcher saw.
    request_id: Option<&'a str>,
    /// #332 B-2 sanitized `pane.process_info` summary probed at dispatch time
    /// while the prompt was still unacknowledged. `None` skips the field.
    foreground_blocker: Option<Value>,
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

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PromptTaskRecord {
    schema_version: u32,
    task_id: String,
    dispatch_id: String,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_target: Option<String>,
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
    /// The `state_change_seq` this task's dispatch was pinned to, when the
    /// pane reported one. An interrupt is only legal while the pane still
    /// sits on this sequence: a later seq means the agent already moved past
    /// this task's turn, so the C-c would land on unrelated work (S5 review
    /// P1-4). Absent on older records, which keep interrupting on the
    /// `working`-status evidence alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_seq: Option<u64>,
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
    /// Correlates this task back to the wire `request_id` on the envelope of
    /// the dispatch call that created it. Absent on records written before S3
    /// and on calls that carried no wire id (direct `tool_call`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    /// 1.2 S5: optional wall-clock run cap from task creation. Expiry emits
    /// an `agent_budget_exceeded` event to the parent; it never terminates the
    /// task by itself. Absent on older records, which keep loading unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_runtime_ms: Option<u64>,
    /// What to do when a budget expires: "notify" (default) or "interrupt".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on_budget: Option<String>,
    /// Durable budget events already emitted for this task, so a restart or a
    /// second tracker pass never re-notifies or re-interrupts. Each entry is
    /// `{event, at_ms, notification_state, [interrupt_state]}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    budget_events: Vec<Value>,
    /// S4 per-receipt repair budget. `repair_of` on a later dispatch records one
    /// attempt here; the receipt turns terminal for repairs at
    /// [`REPAIR_BUDGET_ATTEMPTS`] and further repair redispatches are refused.
    #[serde(default, skip_serializing_if = "is_zero")]
    repair_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repair_exhausted_at_ms: Option<u64>,
    /// #332 B-1 steering lifecycle: how far this prompt has advanced through
    /// `submitted|queued|delivered|acknowledged|applied|superseded|rejected`,
    /// persisted independently from `lifecycle` (which tracks the agent's
    /// turn, not the prompt's adoption). Absent on records written before
    /// B-1; readers derive the same vocabulary from `lifecycle` then.
    /// Once `superseded`, only an own-outcome transition (running/terminal)
    /// may advance it — see `update_steering`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    steering_state: Option<String>,
    /// Task/dispatch id of the newer steering that replaced this record while
    /// it was still unacknowledged. Keeps recording even after the record
    /// reaches its own terminal outcome, so status can tell "this prompt was
    /// superseded by that dispatch" apart from "it applied".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// #332 B-2 foreground blocker: a sanitized `pane.process_info` summary
    /// captured at dispatch while the prompt was still unacknowledged
    /// (`steering_state` queued/delivered/submitted). Always `None` once the
    /// task reports its own outcome. `Value` keeps the contract schema-free;
    /// older runtimes ignore the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    foreground_blocker: Option<Value>,
    #[serde(skip)]
    gate: Option<DecisionGate>,
}

impl PromptTaskRecord {
    fn is_terminal(&self) -> bool {
        self.gate.is_none() && self.lifecycle.is_terminal()
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
    /// #332 B-1 steering facts shared by the full projection and the running
    /// inbox row: the persisted stage when one was written, else the stage
    /// derived from `lifecycle`, plus the superseding task id when recorded.
    fn steering_json(&self) -> (String, Value) {
        let stage = self
            .steering_state
            .clone()
            .unwrap_or_else(|| steering_for_lifecycle(&self.lifecycle).to_owned());
        (stage, json!(self.superseded_by))
    }

    fn public_json_at(&self, now: u64) -> Value {
        let mut public = json!({
            "task_id": self.task_id,
            "dispatch_id": self.dispatch_id,
            "target": self.target,
            "pane_id": self.pane_id,
            "workspace_id": self.workspace_id,
            "lifecycle": self.lifecycle.as_str(),
            "steering_state": self.steering_json().0,
            "superseded_by": self.superseded_by,
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
            // S3 receipt vocabulary: `receipt_id` aliases the task id so a
            // waiting caller can join task rows to the dispatch envelope.
            "receipt_id": self.task_id,
        });
        if let Some(requested) = &self.requested_target {
            public["requested_target"] = json!(requested);
        }
        if let Some(gate) = &self.gate {
            public["blocked_on_gate"] = json!(true);
            public["gate"] = json!(gate);
        }
        if let Some(object) = public.as_object_mut() {
            for (key, value) in [
                ("last_heartbeat_at_ms", self.last_heartbeat_at_ms),
                ("worker_done_at_ms", self.worker_done_at_ms),
                ("stall_after_ms", self.stall_after_ms),
                ("max_runtime_ms", self.max_runtime_ms),
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
            if let Some(on_budget) = &self.on_budget {
                object.insert("on_budget".to_owned(), json!(on_budget));
            }
            if !self.budget_events.is_empty() {
                object.insert("budget_events".to_owned(), json!(self.budget_events));
            }
            // S3: correlate the row back to the dispatch envelope's wire id.
            if let Some(request_id) = &self.request_id {
                object.insert("request_id".to_owned(), json!(request_id));
            }
            if self.repair_attempts > 0 || self.repair_exhausted_at_ms.is_some() {
                object.insert(
                    "repair".to_owned(),
                    json!({
                        "attempts": self.repair_attempts,
                        "limit": REPAIR_BUDGET_ATTEMPTS,
                        "exhausted": self.repair_exhausted_at_ms.is_some(),
                        "exhausted_at_ms": self.repair_exhausted_at_ms,
                    }),
                );
            }
            // #332 B-2: non-secret `pane.process_info` summary while the
            // steering stays unacknowledged; cleared by `update_steering` once
            // the task reports its own outcome.
            if let Some(blocker) = &self.foreground_blocker {
                object.insert("foreground_blocker".to_owned(), json!(blocker));
            }
            // S4: `delivery_uncertain` means "may already have been delivered".
            // The row therefore carries the read-only check a caller can run to
            // find out, instead of a bare doubt. The pane echo cursor is the
            // closest durable evidence this runtime owns; when the pane is
            // unknown the hint still names the surface without inventing one.
            if self.lifecycle == PromptTaskLifecycle::DeliveryUncertain {
                let pane = self.pane_id.clone().or_else(|| Some(self.target.clone()));
                object.insert(
                    "confirm_hint".to_owned(),
                    json!({
                        "action": "herdr_mcp.agent.read",
                        "target": pane,
                        "cursor": self.terminal_cursor,
                        "reason": self.notification_error,
                        "meaning": "the prompt may or may not have reached the agent; read the pane before re-dispatching",
                    }),
                );
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
    role_dispatches: Mutex<()>,
    /// Idempotency keys whose resumable spawn checkpoint is being continued,
    /// so two concurrent replays cannot both prompt the same agent.
    spawn_resumes: Mutex<HashSet<String>>,
    /// Test seam: scripted Herdr fakes answer a fixed request sequence, so a
    /// dispatch test that already scripted create/start/prompt would see the S5
    /// early-exit probe consume a scripted reply. Production never sets it.
    spawn_early_exit_watch_disabled: AtomicBool,
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

/// One in-flight spawn resume. Dropping it releases the key for the next replay.
struct SpawnResumeGuard<'a> {
    registry: &'a PromptRegistry,
    key: String,
}

impl Drop for SpawnResumeGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.registry.inner.spawn_resumes.lock() {
            active.remove(&self.key);
        }
    }
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

    /// Disable the S5 post-readiness early-exit watch for this registry.
    ///
    /// Test-only: a scripted Herdr fake answers one fixed sequence regardless of
    /// method, so the watch's `agent.get` probe would consume a reply the test
    /// scripted for `agent.prompt`. Production never calls this.
    #[cfg(test)]
    pub(crate) fn disable_spawn_early_exit_watch(&self) {
        self.inner
            .spawn_early_exit_watch_disabled
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<Mutex<StateStore>>) -> Self {
        Self {
            inner: Arc::new(PromptRegistryInner {
                task_mutations: Mutex::new(()),
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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

    /// Claim the in-flight resume slot for one idempotency key. `None` means
    /// another replay of the same checkpoint is already continuing it.
    fn begin_spawn_resume(&self, key: &str) -> Option<SpawnResumeGuard<'_>> {
        let mut active = self.inner.spawn_resumes.lock().ok()?;
        if !active.insert(key.to_owned()) {
            return None;
        }
        Some(SpawnResumeGuard {
            registry: self,
            key: key.to_owned(),
        })
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
            requested_target,
            fingerprint,
            idempotency_key,
            before,
            after,
            submitted,
            start_cursor,
            parent_target,
            parent_session_ref,
            stall_after_ms,
            max_runtime_ms,
            on_budget,
            request_id,
            foreground_blocker,
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
        // Pin the dispatch to the pane's observed seq when one was reported;
        // an interrupt later refuses once the pane moves past it.
        let turn_seq = if seq_advanced {
            after_seq
        } else {
            baseline_seq
        };

        let mut record = PromptTaskRecord {
            schema_version: 1,
            task_id: task_id.to_owned(),
            dispatch_id: task_id.to_owned(),
            target: target.to_owned(),
            requested_target: requested_target.map(str::to_owned),
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
            turn_seq,
            exact_native_turn: false,
            terminal_status: None,
            terminal_cursor: None,
            terminal_at_ms: None,
            last_heartbeat_at_ms: None,
            worker_done_at_ms: None,
            worker_done_summary: None,
            stall_after_ms: None,
            request_id: None,
            max_runtime_ms: None,
            on_budget: None,
            budget_events: Vec::new(),
            gate: None,
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
            repair_attempts: 0,
            repair_exhausted_at_ms: None,
            steering_state: None,
            superseded_by: None,
            foreground_blocker,
        };
        if record.lifecycle == PromptTaskLifecycle::DeliveryUncertain {
            mark_delivery_uncertain(&mut record, now, "delivery_state_unknown_at_submit");
        }
        update_steering(&mut record);
        record.stall_after_ms = stall_after_ms;
        record.max_runtime_ms = max_runtime_ms;
        record.on_budget = on_budget.map(str::to_owned);
        record.request_id = request_id.map(str::to_owned);
        self.save_task(&record, Some(fingerprint))?;
        self.supersede_unacknowledged_tasks(&record)?;
        drop(guard);
        let reconciled = self.reconcile_task(task_id)?;
        Ok(reconciled.or(Some(record)))
    }
    /// #332 B-1: every still-unacknowledged steering on this task's lane
    /// (same resolved pane or same unresolved target) is superseded by the
    /// record just persisted. Records already acknowledged (`running`) or
    /// terminal keep their own stage: a new prompt cannot retroactively
    /// unsend work the agent already picked up. Runs under the same
    /// `task_mutations` guard as the task write, before the new record's
    /// reconcile, so a stale snapshot cannot unmark a supersession.
    fn supersede_unacknowledged_tasks(&self, task: &PromptTaskRecord) -> Result<(), String> {
        let now = self.now_ms();
        for mut prior in self.list_active_tasks()? {
            if prior.task_id == task.task_id
                || !task_matches_target(&prior, &task.target, task.pane_id.as_deref())
            {
                continue;
            }
            let prior_stage = prior
                .steering_state
                .clone()
                .unwrap_or_else(|| steering_for_lifecycle(&prior.lifecycle).to_owned());
            if !matches!(prior_stage.as_str(), "submitted" | "queued" | "delivered") {
                continue;
            }
            prior.steering_state = Some("superseded".to_owned());
            prior.superseded_by = Some(task.task_id.clone());
            prior.updated_at_ms = now;
            self.save_task(&prior, None)?;
        }
        Ok(())
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
        let newly_completed = kind == "worker_done" && !task.is_terminal() && task.gate.is_none();
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
                update_steering(&mut task);
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
                    && (task.target == target
                        || task.requested_target.as_deref() == Some(target))
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
            receipt_id: None,
            status: None,
            since_ms: None,
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

    /// S4: one inbox read for a fully populated query (filters included).
    fn task_inbox_with_query(&self, query: &InboxQuery<'_>) -> Value {
        match self.inbox_payload(query) {
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
                    && task.gate.is_none()
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
                json!({
                    "waited_ms": waited_ms,
                    "woke": woke,
                    // Expiry is a checkpoint, not an error: the caller may
                    // re-wait on the same scope; it is not a request timeout.
                    "still_running": woke == "timeout",
                }),
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
    /// S4 per-receipt repair budget.
    ///
    /// A caller that redispatches a repair identifies the receipt it repairs
    /// (`repair_of`). Each accepted repair spends one attempt; at
    /// [`REPAIR_BUDGET_ATTEMPTS`] the receipt's repair lane is terminal and any
    /// further repair is refused as `not_delivered` instead of being submitted.
    /// Repair accounting never changes the receipt's own delivered lifecycle.
    pub fn record_repair_attempt(&self, receipt_id: &str) -> Value {
        let Some(store) = self.inner.store.as_ref() else {
            return json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": "agent task store is not configured",
            });
        };
        let now_ms = self.now_ms();
        let decision = {
            let mut store = match store.lock() {
                Ok(store) => store,
                Err(_) => {
                    return json!({
                        "ok": false,
                        "code": "agent_task_store_unavailable",
                        "message": "agent task store lock unavailable",
                    });
                }
            };
            // One IMMEDIATE transaction owns read -> check -> increment ->
            // write, so concurrent repairs cannot overspend this receipt.
            store.update_operation_result(
                receipt_id,
                PROMPT_TASK_OPERATION_KIND,
                |result_json| {
                    let mut task = decode_task_record_json(result_json)?;
                    if !task.is_terminal() {
                        return Ok((
                            None,
                            json!({
                                "ok": false,
                                "code": "repair_target_not_terminal",
                                "receipt_id": receipt_id,
                                "delivery_state": "not_delivered",
                                "retryable": false,
                                "message": "only a terminal receipt can be repaired",
                            }),
                        ));
                    }
                    if task.repair_exhausted_at_ms.is_some()
                        || task.repair_attempts >= REPAIR_BUDGET_ATTEMPTS
                    {
                        let mut exhausted = json!({
                            "ok": false,
                            "code": "repair_budget_exhausted",
                            "receipt_id": receipt_id,
                            "attempts": task.repair_attempts,
                            "limit": REPAIR_BUDGET_ATTEMPTS,
                            "terminal": true,
                            "delivery_state": "not_delivered",
                            "retryable": false,
                            "fallback_allowed": false,
                            "hint": "this receipt's repair lane is terminal; start a fresh dispatch instead of repairing it again",
                        });
                        if task.repair_exhausted_at_ms.is_none() {
                            task.repair_exhausted_at_ms = Some(now_ms);
                            let encoded = serde_json::to_string(&task)
                                .map_err(|error| format!("cannot encode agent task: {error}"))?;
                            exhausted["attempts"] = json!(task.repair_attempts);
                            return Ok((Some(encoded), exhausted));
                        }
                        return Ok((None, exhausted));
                    }
                    task.repair_attempts = task.repair_attempts.saturating_add(1);
                    let encoded = serde_json::to_string(&task)
                        .map_err(|error| format!("cannot encode agent task: {error}"))?;
                    Ok((
                        Some(encoded),
                        json!({
                            "ok": true,
                            "receipt_id": receipt_id,
                            "attempts": task.repair_attempts,
                            "limit": REPAIR_BUDGET_ATTEMPTS,
                            "remaining": REPAIR_BUDGET_ATTEMPTS.saturating_sub(task.repair_attempts),
                        }),
                    ))
                },
            )
        };
        match decision {
            Ok(Some(value)) => value,
            Ok(None) => json!({
                "ok": false,
                "code": "agent_task_not_found",
                "receipt_id": receipt_id,
            }),
            Err(error) => json!({
                "ok": false,
                "code": "agent_task_store_unavailable",
                "message": error,
            }),
        }
    }

    /// Terminal receipts whose retention window elapsed. They are hidden from
    /// default queries and were never deleted, so this is the archive surface.
    fn list_archived_tasks(&self, limit: usize) -> Result<Vec<PromptTaskRecord>, String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(vec![]);
        };
        let store = store
            .lock()
            .map_err(|_| "agent task store lock unavailable".to_owned())?;
        let rows = store.operation_ledgers_archived(PROMPT_TASK_OPERATION_KIND, limit)?;
        drop(store);
        rows.into_iter()
            .map(|record| {
                let mut task = decode_task_record(&record)?;
                if let Some(store) = self.inner.store.as_ref() {
                    let store = store
                        .lock()
                        .map_err(|_| "agent task store lock unavailable".to_owned())?;
                    task.gate = store.open_decision_gate(&task.task_id)?;
                }
                Ok(task)
            })
            .collect()
    }

    fn inbox_scope_matches(&self, task: &PromptTaskRecord, query: &InboxQuery) -> bool {
        task_matches_scope(
            task,
            query.workspace_id,
            query.parent_target,
            query.parent_session_ref,
        ) && task_matches_receipt_filters(task, query.receipt_id, query.status, query.since_ms)
    }

    /// S4 read-only counters for one inbox scope. No mutation, no task bodies.
    fn inbox_stats(&self, tasks: &[PromptTaskRecord], query: &InboxQuery) -> Value {
        let mut running = 0_u64;
        let mut completed = 0_u64;
        let mut blocked = 0_u64;
        let mut failed = 0_u64;
        let mut uncertain = 0_u64;
        let mut oldest_updated_ms = None::<u64>;
        let mut newest_updated_ms = None::<u64>;
        for task in tasks
            .iter()
            .filter(|task| self.inbox_scope_matches(task, query))
        {
            match task.lifecycle {
                PromptTaskLifecycle::Running
                | PromptTaskLifecycle::AwaitingActivity
                | PromptTaskLifecycle::AwaitingPriorSettle => running += 1,
                PromptTaskLifecycle::DeliveryUncertain => uncertain += 1,
                PromptTaskLifecycle::Completed => completed += 1,
                PromptTaskLifecycle::Blocked => blocked += 1,
                PromptTaskLifecycle::Failed => failed += 1,
            }
            oldest_updated_ms = Some(
                oldest_updated_ms.map_or(task.updated_at_ms, |value| value.min(task.updated_at_ms)),
            );
            newest_updated_ms = Some(
                newest_updated_ms.map_or(task.updated_at_ms, |value| value.max(task.updated_at_ms)),
            );
        }
        let archived = self
            .list_archived_tasks(512)
            .map(|rows| {
                rows.iter()
                    .filter(|task| task.is_terminal() && self.inbox_scope_matches(task, query))
                    .count() as u64
            })
            .unwrap_or(0);
        json!({
            "read_only": true,
            "scope": {
                "workspace_id": query.workspace_id,
                "parent_scoped": query.parent_target.is_some()
                    || query.parent_session_ref.is_some(),
            },
            "active": running,
            "completed": completed,
            "blocked": blocked,
            "failed": failed,
            "delivery_uncertain": uncertain,
            "terminal": completed + blocked + failed + uncertain,
            "archived": archived,
            "oldest_updated_ms": oldest_updated_ms,
            "newest_updated_ms": newest_updated_ms,
            "retention_ms": prompt_task_ttl_ms(),
        })
    }

    /// S4 archive read: terminal receipts past retention, within the caller's
    /// scope and filters. Default queries never include these rows.
    fn archive_payload(&self, query: &InboxQuery) -> Value {
        // The embedded stats describe the caller's live scope as well as the
        // archive: computing them from an empty set would report `active: 0` /
        // `terminal: 0` for a scope that still has live receipts (S4 review P2).
        let live = self.list_tasks(512).unwrap_or_default();
        let archived = match self.list_archived_tasks(512) {
            Ok(archived) => archived,
            Err(error) => {
                return json!({
                    "ok": false,
                    "code": "agent_task_store_unavailable",
                    "message": error,
                });
            }
        };
        let mut tasks = archived
            .into_iter()
            .filter(|task| task.is_terminal() && self.inbox_scope_matches(task, query))
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at_ms));
        tasks.truncate(query.limit.clamp(1, 512));
        json!({
            "ok": true,
            "archive": true,
            "retention_ms": prompt_task_ttl_ms(),
            "tasks": tasks
                .into_iter()
                .map(|task| task.public_json_at(self.now_ms()))
                .collect::<Vec<_>>(),
            "stats": self.inbox_stats(&live, query),
        })
    }

    /// S4 stats-only view: counters for the caller's scope, no task rows.
    fn stats_payload(&self, query: &InboxQuery) -> Value {
        let tasks = match self.list_tasks(512) {
            Ok(tasks) => tasks,
            Err(error) => {
                return json!({
                    "ok": false,
                    "code": "agent_task_store_unavailable",
                    "message": error,
                });
            }
        };
        json!({
            "ok": true,
            "stats": self.inbox_stats(&tasks, query),
        })
    }

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
            in_scope(task)
                && task_matches_receipt_filters(
                    task,
                    query.receipt_id,
                    query.status,
                    query.since_ms,
                )
                && task.is_terminal()
                && task.acknowledged_at_ms.is_none()
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
            receipt_id,
            status,
            since_ms,
        } = *query;
        self.reconcile_active_tasks();
        let tasks = self.list_tasks(512)?;
        let scoped = tasks
            .iter()
            .filter(|task| {
                task_matches_scope(task, workspace_id, parent_target, parent_session_ref)
            })
            .filter(|task| task_matches_receipt_filters(task, receipt_id, status, since_ms))
            .filter(|task| {
                !task.is_terminal() || include_acknowledged || task.acknowledged_at_ms.is_none()
            })
            .collect::<Vec<_>>();

        let mut terminal = scoped
            .iter()
            .copied()
            .filter(|task| task.is_terminal() || task.gate.is_some())
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
            if task.gate.is_some() {
                blocked += 1;
                continue;
            }
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
            // S4 read-only governance stats for this scope. Counts only; the
            // archive count comes from rows whose retention window elapsed.
            "stats": self.inbox_stats(&tasks, query),
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
                if task.gate.is_some() {
                    return json!({"ok": false, "code": "task_blocked_on_gate"});
                }
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
                            "still_running": true,
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
                        registry.sweep_task_budgets();
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

    /// 1.2 S5: emit budget events for non-terminal tasks whose explicit
    /// `stall_after_ms` or `max_runtime_ms` has expired. Notify-only by
    /// default; `on_budget:"interrupt"` additionally sends one explicit C-c.
    /// Each event is recorded durably before delivery so a restart or a second
    /// pass never re-notifies or re-interrupts. Tasks without caps are skipped
    /// entirely (the default 20-minute stall liveness projection is unchanged
    /// and emits nothing). Never terminates a task.
    fn sweep_task_budgets(&self) {
        // Active tasks only: the bounded recent-512 window can evict a still
        // running task behind newer terminal history and silence its caps
        // forever (S5 review P1-5).
        let Ok(tasks) = self.list_active_tasks() else {
            return;
        };
        let now = self.now_ms();
        for task in tasks {
            if task.is_terminal() {
                continue;
            }
            // The event row is persisted as `pending` before delivery. A crash
            // in that window would otherwise leave it pending forever, because
            // the row's existence suppresses a fresh event. Re-deliver pending
            // rows on the next sweep (at-least-once recovery, same intent as
            // the pending terminal notification): a duplicate parent message is
            // preferable to a parent that never learns the cap expired.
            // The interrupt is different: an undelivered C-c must never be
            // replayed, so recovery passes `interrupt=false` and the delivery
            // pass finalizes a leftover `pending` interrupt as `uncertain`
            // instead of sending a second keystroke (S5 review P2-1).
            let pending: Vec<String> = task
                .budget_events
                .iter()
                .filter(|row| {
                    row.get("notification_state").and_then(Value::as_str) == Some("pending")
                })
                .filter_map(|row| row.get("event").and_then(Value::as_str).map(str::to_owned))
                .collect();
            for event in pending {
                self.deliver_budget_event(&task.task_id, &event, now, false);
            }
            let mut due: Vec<&'static str> = Vec::new();
            if let Some(stall) = task.stall_after_ms
                && now.saturating_sub(task.updated_at_ms) >= stall
                && !task.budget_events.iter().any(|event| {
                    event.get("event").and_then(Value::as_str) == Some("agent_stalled")
                })
            {
                due.push("agent_stalled");
            }
            if let Some(cap) = task.max_runtime_ms
                && now.saturating_sub(task.created_at_ms) >= cap
                && !task.budget_events.iter().any(|event| {
                    event.get("event").and_then(Value::as_str) == Some("agent_budget_exceeded")
                })
            {
                due.push("agent_budget_exceeded");
            }
            for event in due {
                self.emit_budget_event(&task.task_id, event, now);
            }
        }
    }

    /// Record one budget event under the task mutation lock, then deliver the
    /// parent notification (and optional interrupt) outside the lock. The
    /// event row is persisted first with `notification_state:"pending"`; the
    /// delivery outcome updates that row, mirroring terminal notifications.
    fn emit_budget_event(&self, task_id: &str, event: &str, now: u64) {
        let Ok(_guard) = self.inner.task_mutations.lock() else {
            return;
        };
        let Ok(Some(mut task)) = self.load_task(task_id) else {
            return;
        };
        if task.is_terminal()
            || task
                .budget_events
                .iter()
                .any(|row| row.get("event").and_then(Value::as_str) == Some(event))
        {
            return;
        }
        // Interrupt eligibility is a one-shot, durably reserved at emit time
        // under the task mutation lock: a second due event (stall + runtime
        // cap expiring together, or a later threshold on the same task) may
        // still notify the parent but must never produce a second C-c
        // (S5 review P1-3).
        let interrupt_reserved = task.on_budget.as_deref() == Some("interrupt")
            && !task.budget_events.iter().any(|row| {
                row.get("interrupt_state").and_then(Value::as_str) != Some("not_requested")
                    && row.get("interrupt_state").and_then(Value::as_str).is_some()
            });
        task.budget_events.push(json!({
            "event": event,
            "at_ms": now,
            "notification_state": "pending",
            "interrupt_state": if interrupt_reserved { "pending" } else { "not_requested" },
        }));
        // The delivery call below interrupts only for the row that reserved
        // eligibility above.
        // Budget events are observations, not activity: do not touch
        // updated_at_ms, or a stall event would reset its own stall clock.
        if self.save_task(&task, None).is_err() {
            return;
        }
        drop(_guard);
        self.deliver_budget_event(task_id, event, now, interrupt_reserved);
    }

    fn deliver_budget_event(&self, task_id: &str, event: &str, now: u64, interrupt: bool) {
        let Some(client) = self.inner.client.as_ref() else {
            return;
        };
        let Ok(Some(task)) = self.load_task(task_id) else {
            return;
        };
        let mut notification_state = "not_attempted".to_owned();
        let mut notification_error: Option<String> = None;
        if let Some(parent_target) = task.parent_target.as_deref() {
            let text = format!(
                "Herdr child-task budget event: {event} for task {} (child {}). The task is still running; this is a notification, not a completion. Inspect it with agent status/inbox before deciding to wait, interrupt, or re-dispatch.",
                task.task_id, task.target
            );
            match client.call_with_timeout(
                "agent.prompt",
                json!({"target": parent_target, "text": text}),
                Duration::from_secs(10),
            ) {
                Ok(_) => notification_state = "delivered".to_owned(),
                Err(error) => {
                    notification_state = if definitely_not_delivered(&error) {
                        "not_delivered".to_owned()
                    } else {
                        "uncertain".to_owned()
                    };
                    notification_error = Some(truncate_chars(&error.message, 512));
                }
            }
        }
        // A recovered sweep passes `interrupt=false`: never replay a C-c whose
        // first delivery crashed between persist and send. The row's `pending`
        // interrupt is finalized as `uncertain` below (S5 review P2-1).
        let interrupt_state = if interrupt {
            Some(self.interrupt_task_pane(client, &task))
        } else {
            None
        };
        let Ok(_guard) = self.inner.task_mutations.lock() else {
            return;
        };
        let Ok(Some(mut task)) = self.load_task(task_id) else {
            return;
        };
        if let Some(row) = task
            .budget_events
            .iter_mut()
            .find(|row| row.get("event").and_then(Value::as_str) == Some(event))
        {
            row["notification_state"] = json!(notification_state);
            row["interrupt_state"] = json!(match interrupt_state {
                Some(state) => state,
                None => {
                    if row.get("interrupt_state").and_then(Value::as_str) == Some("pending") {
                        "uncertain".to_owned()
                    } else {
                        row["interrupt_state"]
                            .as_str()
                            .unwrap_or("not_requested")
                            .to_owned()
                    }
                }
            });
            if let Some(error) = notification_error {
                row["notification_error"] = json!(error);
            }
            let _ = now;
        }
        let _ = self.save_task(&task, None);
    }

    /// One explicit C-c to the task's own pane, only when an agent is actively
    /// working on *this task's* turn (mirrors browser_control::execute_interrupt's
    /// guard plus the durable ownership checks below). Never sends to a parent,
    /// an idle pane, a task that already settled, or a pane that moved past
    /// the seq this task was pinned to (S5 review P1-4).
    fn interrupt_task_pane(&self, client: &HerdrClient, task: &PromptTaskRecord) -> String {
        let Some(pane_id) = task.pane_id.as_deref() else {
            return "no_pane".to_owned();
        };
        let Some(state) = agent_state_of(client, pane_id) else {
            return "agent_gone".to_owned();
        };
        if state.agent_status.as_deref() != Some("working") {
            return "no_active_turn".to_owned();
        }
        // Side-effect fence, re-checked at send time: the notification call
        // above can span seconds, during which a worker_done/terminal event
        // may settle this task or the pane may start an unrelated new turn.
        // Reload the durable record and require the pane to still sit on the
        // seq this task was pinned to before touching the keyboard; anything
        // that cannot prove ownership fails closed (S5 review P1-4).
        match self.load_task(&task.task_id) {
            Ok(Some(fresh)) => {
                if fresh.is_terminal() {
                    return "task_settled".to_owned();
                }
                match (fresh.turn_seq, state.state_change_seq) {
                    (Some(pinned), Some(live)) if live == pinned => {}
                    (Some(_), Some(_)) => return "turn_mismatch".to_owned(),
                    _ => return "turn_unverifiable".to_owned(),
                }
            }
            // A record that vanished or a store that cannot answer cannot
            // prove the pane still belongs to this task.
            _ => return "turn_unverifiable".to_owned(),
        }
        match client.call_with_timeout(
            "pane.send_keys",
            json!({"pane_id": pane_id, "keys": ["C-c"]}),
            Duration::from_secs(5),
        ) {
            Ok(_) => "sent".to_owned(),
            Err(error) => {
                if definitely_not_delivered(&error) {
                    "not_delivered".to_owned()
                } else {
                    "uncertain".to_owned()
                }
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
        if !trigger.is_terminal() || trigger.notification_state != "pending" {
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
        if task.gate.is_some() || task.is_terminal() {
            return Ok(Some(task));
        }
        // A report received during a gate is durable, but completes only after
        // the gate is released. This explicit report does not depend on cache cursors.
        if let Some(done_at) = task.worker_done_at_ms {
            task.lifecycle = PromptTaskLifecycle::Completed;
            task.terminal_status = Some("worker_done".to_owned());
            task.terminal_at_ms = Some(done_at);
            task.terminal_cursor = None;
            task.awaiting_children = None;
            task.notification_state = "pending".to_owned();
            update_steering(&mut task);
            self.save_task(&task, None)?;
            self.maybe_notify_parent_locked(&task);
            return self.load_task(task_id);
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
        if task.gate.is_some() {
            return Ok(false);
        }
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
        let mut task = decode_task_record(&record)?;
        task.gate = store.open_decision_gate(task_id)?;
        Ok(Some(task))
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
            .map(|record| {
                let mut task = decode_task_record(&record)?;
                task.gate = store.open_decision_gate(&task.task_id)?;
                Ok(task)
            })
            .collect()
    }

    /// Every task whose ledger row is still non-terminal, unbounded by the
    /// recent-window cap: the budget sweep must see a long-running task even
    /// when hundreds of newer terminal tasks were written after it (S5
    /// review P1-5). Open gates stay live the same way `list_tasks` sees
    /// them, so gated tasks still appear here.
    fn list_active_tasks(&self) -> Result<Vec<PromptTaskRecord>, String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(vec![]);
        };
        let store = store
            .lock()
            .map_err(|_| "agent task store lock unavailable".to_owned())?;
        store
            .operation_ledgers_by_phase(PROMPT_TASK_OPERATION_KIND, "active")?
            .into_iter()
            .map(|record| {
                let mut task = decode_task_record(&record)?;
                task.gate = store.open_decision_gate(&task.task_id)?;
                Ok(task)
            })
            .collect()
    }

    fn save_task(&self, task: &PromptTaskRecord, request_hash: Option<&str>) -> Result<(), String> {
        let Some(store) = self.inner.store.as_ref() else {
            return Ok(());
        };
        let encoded = serde_json::to_string(task)
            .map_err(|error| format!("cannot encode agent task: {error}"))?;
        let now = now_ms();
        let expires_at = now.saturating_add(prompt_task_ttl_ms());
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
    update_steering(task);
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
    /// S4 filters. `receipt_id` aliases the task id, `status` matches the
    /// lifecycle/terminal status string, `since_ms` keeps receipts updated at or
    /// after that millisecond timestamp.
    receipt_id: Option<&'a str>,
    status: Option<&'a str>,
    since_ms: Option<u64>,
}

/// S4 receipt filters, applied identically to the default inbox, the archive
/// and the bounded wait. `status` accepts the lifecycle name (`completed`,
/// `failed`, `blocked`, `delivery_uncertain`, `running`, ...) or the recorded
/// terminal status.
fn task_matches_receipt_filters(
    task: &PromptTaskRecord,
    receipt_id: Option<&str>,
    status: Option<&str>,
    since_ms: Option<u64>,
) -> bool {
    receipt_id.is_none_or(|receipt_id| task.task_id == receipt_id)
        && status.is_none_or(|status| {
            task.lifecycle.as_str() == status || task.terminal_status.as_deref() == Some(status)
        })
        && since_ms.is_none_or(|since_ms| task.updated_at_ms >= since_ms)
}

/// Inbox task projection: `full` keeps today's payload, `compact` drops the
/// fields a supervising planner does not need for a round trip.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum InboxView {
    Full,
    Compact,
    /// S4: terminal receipts past retention. They left the default queries but
    /// were never deleted, so they stay queryable here.
    Archive,
    /// S4: read-only counters for the caller's scope; no task rows.
    Stats,
}

/// Fields kept by `view: compact`. Attention facts are only carried when the
/// task object already exposes them; the raw `parent_session_ref` never is.
/// `final_excerpt` is present only when captured and spares the planner an
/// `agent.read` round trip; `awaiting_children` is present only while a
/// supervisor is held for its own children.
const COMPACT_TASK_FIELDS: [&str; 19] = [
    "task_id",
    "receipt_id",
    "request_id",
    "dispatch_id",
    "requested_target",
    "target",
    "pane_id",
    "lifecycle",
    "steering_state",
    "superseded_by",
    "liveness",
    "terminal",
    "acknowledged",
    "terminal_at_ms",
    "updated_at_ms",
    "final_excerpt",
    "awaiting_children",
    "gate",
    "blocked_on_gate",
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
        "receipt_id": task.task_id,
        "request_id": task.request_id,
        "dispatch_id": task.dispatch_id,
        "workspace_id": task.workspace_id,
        "lifecycle": task.lifecycle.as_str(),
        "liveness": task.liveness(now),
        "terminal": false,
        "steering_state": task.steering_json().0,
        "superseded_by": task.superseded_by,
    });
    if let Some(requested) = &task.requested_target {
        row["target"] = json!(task.target);
        row["requested_target"] = json!(requested);
    }
    if let (Some(awaiting_children), Some(object)) = (task.awaiting_children, row.as_object_mut()) {
        object.insert("awaiting_children".to_owned(), json!(awaiting_children));
    }
    row
}

fn apply_inbox_view(payload: Value, view: InboxView) -> Value {
    if !matches!(view, InboxView::Compact) {
        // `full`, `archive` and `stats` payloads are already final: archive
        // rows keep every fact a caller may need to reconcile a receipt.
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
        Some("archive") => Ok(InboxView::Archive),
        Some("stats") => Ok(InboxView::Stats),
        _ => Err(invalid(
            r#"view must be "full", "compact", "archive" or "stats""#,
        )),
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

/// Decode one task payload without a ledger row, for atomic in-transaction
/// read-modify-write paths (`update_operation_result`).
fn decode_task_record_json(encoded: &str) -> Result<PromptTaskRecord, String> {
    serde_json::from_str(encoded).map_err(|error| format!("cannot decode agent task: {error}"))
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
        update_steering(task);
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
            update_steering(task);
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
            // The task's live turn is the seq the pane entered working on;
            // re-pin the interrupt fence here so a dispatch-time baseline
            // cannot hold the interrupt back, and a later unrelated turn
            // still refuses (S5 review P1-4).
            if let Some(seq) = task_event_state_change_seq(event) {
                task.turn_seq = Some(seq);
            }
            task.updated_at_ms = now;
            update_steering(task);
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
            update_steering(task);
            true
        }
        "idle" | "done" if task.activity_observed => {
            task.lifecycle = PromptTaskLifecycle::Completed;
            task.terminal_status = Some(status.to_owned());
            task.terminal_cursor = Some(cursor);
            task.terminal_at_ms = Some(now);
            task.notification_state = "pending".to_owned();
            task.updated_at_ms = now;
            update_steering(task);
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
    update_steering(task);
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
        OperationReservation::CapacityExhausted => {
            unreachable!("capacity admission is scoped to browser_page.action")
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
        decision_gates::RESOLVE => json!({"ok": false, "code": "gate_resolution_requires_human"}),
        decision_gates::CREATE | decision_gates::STATUS | decision_gates::CANCEL => {
            registry.gate_call(method, params, caller_parent_session_ref)
        }
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
            // S4 receipt filters: `receipt_id` aliases the task id, `status`
            // matches the lifecycle/terminal status, `since_ms` keeps receipts
            // updated at or after that timestamp. They apply identically to the
            // default inbox, the bounded wait, the archive and the stats view.
            let receipt_id = match params.get("receipt_id") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(value)
                        if !value.is_empty()
                            && value.len() <= 128
                            && !value.chars().any(char::is_control) =>
                    {
                        Some(value)
                    }
                    _ => {
                        return invalid("receipt_id must be a non-empty string up to 128 bytes");
                    }
                },
            };
            let status = match params.get("status") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(value)
                        if !value.is_empty()
                            && value.len() <= 64
                            && !value.chars().any(char::is_control) =>
                    {
                        Some(value)
                    }
                    _ => return invalid("status must be a non-empty string up to 64 bytes"),
                },
            };
            let since_ms = match params.get("since_ms") {
                None => None,
                Some(value) => match value.as_u64() {
                    Some(value) => Some(value),
                    None => return invalid("since_ms must be a non-negative integer"),
                },
            };
            let query = InboxQuery {
                workspace_id,
                parent_target,
                parent_session_ref,
                include_acknowledged,
                advisory,
                limit,
                receipt_id,
                status,
                since_ms,
            };
            let inbox = match view {
                InboxView::Archive => registry.archive_payload(&query),
                InboxView::Stats => registry.stats_payload(&query),
                _ => match wait_ms {
                    Some(wait_ms) => registry.task_inbox_waiting(&query, wait_ms),
                    None => registry.task_inbox_with_query(&query),
                },
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
    run_with_parent_session(client, registry, args, None, None)
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
    request_id: Option<&str>,
) -> Value {
    if let Err(error) = mutation::check_global("herdr_prompt") {
        return error;
    }
    // `spawn` is parsed before the target: a spawn dispatch has no target yet,
    // and every rejection below must land before the runtime reserves a key,
    // creates a pane, or starts an agent.
    let spawn = match parse_spawn(args) {
        Ok(spawn) => spawn,
        Err(error) => return error,
    };
    let target = match &spawn {
        Some(_) => {
            if args.get("target").is_some() {
                return spawn_invalid(
                    "target and spawn are mutually exclusive: use spawn to create a new agent, or target to reuse an existing one",
                );
            }
            ""
        }
        None => match required_str(args, "target") {
            Ok("") => return invalid("target must not be empty"),
            Ok(value) => value,
            Err(error) => return error,
        },
    };
    // S4: an explicit repair redispatch names the terminal receipt it repairs.
    // The per-receipt budget is spent before any socket call, so an exhausted
    // budget is `not_delivered` by construction (nothing was submitted).
    let repair_of = match args.get("repair_of") {
        None => None,
        Some(value) => match value.as_str() {
            Some(value)
                if !value.is_empty()
                    && value.len() <= 128
                    && !value.chars().any(char::is_control) =>
            {
                Some(value)
            }
            _ => return invalid("repair_of must be a non-empty receipt id up to 128 bytes"),
        },
    };
    if repair_of.is_some() && spawn.is_some() {
        return spawn_invalid(
            "repair_of cannot be combined with spawn: a repair redispatches to an existing target",
        );
    }
    let role = match role_targets::RoleTarget::parse(target) {
        Ok(role) => role,
        Err(error) => return error,
    };
    let workspace_id = match optional_str(args, "workspace_id") {
        Ok(value) => value,
        Err(error) => return error,
    };
    if spawn.is_some() && workspace_id.is_some() {
        return spawn_invalid(
            "workspace_id is not applicable with spawn; pass spawn.workspace_id instead",
        );
    }
    let max_concurrent = match args.get("max_concurrent") {
        None => None,
        Some(value) => match value.as_u64() {
            Some(value) if (1..=16).contains(&value) => Some(value),
            _ => return invalid("max_concurrent must be an integer in 1..=16"),
        },
    };
    if max_concurrent.is_some() && role.is_none() {
        return if spawn.is_some() {
            spawn_invalid("max_concurrent is not applicable with spawn")
        } else {
            invalid("max_concurrent requires an explicit role selector target")
        };
    }
    let text = match required_str(args, "text") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let idempotency_key = match optional_str(args, "idempotency_key") {
        Ok(value) => value,
        Err(error) => return error,
    };
    if spawn.is_some() && idempotency_key.is_none() {
        return spawn_invalid(
            "idempotency_key is required with spawn: a replay must never create a second pane, tab or agent",
        );
    }
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
    // 1.2 S5: optional run cap and budget action. Like stall_after_ms these
    // are observation/governance preferences, not part of the mutation
    // fingerprint; expiry notifies (or explicitly interrupts) but never
    // terminates the task on its own.
    let max_runtime_ms = match args.get("max_runtime_ms") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(value) if (60_000..=24 * 60 * 60_000).contains(&value) => Some(value),
            _ => return invalid("max_runtime_ms must be an integer in 60000..=86400000"),
        },
    };
    let on_budget = match args.get("on_budget") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value == "notify" || value == "interrupt" => {
            Some(value.as_str())
        }
        Some(_) => return invalid("on_budget must be \"notify\" or \"interrupt\""),
    };
    if on_budget.is_some() && max_runtime_ms.is_none() && stall_after_ms.is_none() {
        return invalid("on_budget requires max_runtime_ms or stall_after_ms");
    }
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
    // A spawn fingerprints its canonical spawn intent, never the pane it creates.
    let fingerprint = match &spawn {
        Some(spawn) => spawn.request_fingerprint(text),
        None => request_fingerprint(target, text),
    };
    let dispatch_id = dispatch_id(idempotency_key, &fingerprint);
    let task_id = dispatch_id.as_str();
    let mut operation_id = None;
    // A spawn reserves its key before resolving or creating anything: a
    // concurrent call must fail closed as in-flight, and a replay must return
    // the stored outcome even when the live layout changed since.
    let mut spawn_resume: Option<SpawnResume> = None;
    if spawn.is_some()
        && let Some(key) = idempotency_key
    {
        match reserve_dispatch(registry, key, &fingerprint, task_id, wait.as_ref()) {
            DispatchReservation::Reserved(op_id) => operation_id = op_id,
            DispatchReservation::Known(result) => match spawn_checkpoint(&result) {
                // A chain that paused on the caller's budget resumes from its
                // persisted pane/agent under the same key instead of spawning.
                Some(checkpoint) => spawn_resume = Some(checkpoint),
                None => return result,
            },
        }
    }
    // S5 review P1-2: model admission runs only after the durable record was
    // consulted. A replay of an already-recorded outcome returns above and
    // never probes the live model list or schema; a resumed chain whose agent
    // is still pending re-uses the persisted delivery form and only
    // re-admits when the checkpoint predates the field.
    let mut spawn = spawn;
    if let Some(spawn) = spawn.as_mut() {
        match spawn_resume
            .as_ref()
            .map(|resume| resume.model_deliverable.clone())
        {
            Some(Some(deliverable)) => spawn.model_deliverable = Some(deliverable),
            Some(None) if spawn_resume.is_some() => {
                // Checkpoint written before the delivery form was persisted:
                // re-run admission rather than drop the requested model.
                if let Err(refusal) = admit_spawn_model(spawn) {
                    return finish_spawn_failure(
                        registry,
                        idempotency_key,
                        &fingerprint,
                        task_id,
                        refusal,
                    );
                }
            }
            None => {
                if let Err(refusal) = admit_spawn_model(spawn) {
                    return finish_spawn_failure(
                        registry,
                        idempotency_key,
                        &fingerprint,
                        task_id,
                        refusal,
                    );
                }
            }
            _ => {}
        }
    }
    // Resolve the spawn plan (workspace, cwd, agent name) before any mutation:
    // an invalid plan must never leave a pane or an agent behind, and its
    // rejection is persisted so the key replays it instead of spawning later.
    let spawn_dispatch = match (&spawn, idempotency_key) {
        (Some(spawn), Some(key)) => match resolve_spawn_dispatch(registry, spawn, key) {
            Ok(dispatch) => Some(dispatch),
            Err(failure) => {
                return finish_spawn_failure(
                    registry,
                    idempotency_key,
                    &fingerprint,
                    task_id,
                    failure,
                );
            }
        },
        (Some(_), None) => return spawn_invalid("idempotency_key is required with spawn"),
        (None, _) => None,
    };

    // Delegation depth is decidable from the task ledger, so a spawn dispatch
    // that would create a third-level worker is refused before a pane or agent
    // exists: nothing to reclaim and no phantom prompt step. The new pane can
    // never be the caller itself, so only the ledger depth check applies here.
    if spawn_dispatch.is_some()
        && let Some(parent_target) = parent_target
        && let Some(mut rejection) = delegation_guard(client, registry, "", None, parent_target)
    {
        if spawn_resume.is_some() {
            // A resumed chain already owns a persisted pane/agent outcome: this
            // refusal is a live observation about the parent chain, so it must
            // not overwrite the checkpoint this key replays from.
            insert_dispatch_id(&mut rejection, task_id);
            insert_task_id(&mut rejection, task_id);
            return rejection;
        }
        // This call reserved the key above and created nothing, so the refusal
        // is this key's outcome: persist it like every other pre-pane failure.
        // Otherwise the reservation stays pending and a same-key retry inside
        // the TTL observes `idempotency_in_flight` instead of the refusal.
        return finish_spawn_failure(registry, idempotency_key, &fingerprint, task_id, rejection);
    }

    // Two concurrent replays of one checkpoint must not both prompt.
    let _resume_guard = match (&spawn_resume, idempotency_key) {
        (Some(checkpoint), Some(key)) => match registry.begin_spawn_resume(key) {
            Some(guard) => Some(guard),
            None => {
                let mut busy = spawn_resume_in_progress(checkpoint);
                insert_dispatch_id(&mut busy, task_id);
                insert_task_id(&mut busy, task_id);
                return busy;
            }
        },
        _ => None,
    };

    // A parented dispatch probes the caller's own agent once for the delegation
    // guards and reuses that observation as the pre-submit baseline below.
    // Hold admission through durable task registration, including requests with
    // no cap, so concurrent role calls cannot race a capped caller's count.
    let role_admission = if role.is_some() {
        match registry.inner.role_dispatches.lock() {
            Ok(guard) => Some(guard),
            Err(_) => return role_targets::not_delivered("agent_task_store_unavailable"),
        }
    } else {
        None
    };
    let guard_observation = if role.is_none() && !target.is_empty() {
        parent_target.and_then(|_| agent_state_of(client, target))
    } else {
        None
    };
    if role.is_none()
        // A spawn has no pane yet; its delegation guard runs after the pane
        // and agent exist (see below).
        && !target.is_empty()
        && let Some(parent_target) = parent_target
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

    if spawn.is_none()
        && let Some(key) = idempotency_key
    {
        match reserve_dispatch(registry, key, &fingerprint, task_id, wait.as_ref()) {
            DispatchReservation::Reserved(op_id) => operation_id = op_id,
            DispatchReservation::Known(result) => return result,
        }
    }

    let requested_target = role.as_ref().map(|_| target);
    let resolution = if let Some(role) = role.as_ref() {
        match role.resolve(
            client,
            registry,
            workspace_id,
            parent_target,
            parent_session_ref,
            max_concurrent,
        ) {
            Ok(resolution) => Some(resolution),
            Err(mut error) => {
                insert_dispatch_id(&mut error, task_id);
                insert_task_id(&mut error, task_id);
                if let Some(key) = idempotency_key
                    && let Err(persist_error) = registry.complete(key, &fingerprint, &error)
                {
                    return annotate_idempotency_persistence(error, persist_error);
                }
                return error;
            }
        }
    } else {
        None
    };
    // S4 repair budget (droid P1-1): spend it only after the whole request has
    // been parsed, validated and admitted. An invalid `text`/`wait`/
    // `stall_after_ms`/lease or a rejected role resolution must never burn an
    // attempt, and a refusal here is still `not_delivered` because nothing has
    // been submitted yet.
    if let Some(receipt_id) = repair_of {
        let mut budget = registry.record_repair_attempt(receipt_id);
        if budget.get("ok").and_then(Value::as_bool) != Some(true) {
            if let Some(object) = budget.as_object_mut() {
                object.insert("dispatch_id".to_owned(), json!(task_id));
            }
            // Persist the refusal under this key like every other pre-delivery
            // failure, so a same-key retry replays it instead of staying in
            // flight until the reservation TTL.
            if let Some(key) = idempotency_key
                && let Err(persist_error) = registry.complete(key, &fingerprint, &budget)
            {
                return annotate_idempotency_persistence(budget, persist_error);
            }
            return budget;
        }
    }

    // A spawn resolves its target by creating it, or by resuming the pane a
    // resumable checkpoint already persisted.
    let mut spawn_outcome = None;
    let dispatched_target = match spawn_dispatch.as_ref() {
        Some(dispatch) => match spawn_chain(client, registry, dispatch, spawn_resume.as_ref()) {
            Ok(outcome) => {
                let pane_id = outcome.pane_id.clone();
                spawn_outcome = Some(outcome);
                pane_id
            }
            Err(failure) => {
                return finish_spawn_failure(
                    registry,
                    idempotency_key,
                    &fingerprint,
                    task_id,
                    failure,
                );
            }
        },
        None => resolution
            .as_ref()
            .map_or(target, |resolved| resolved.target.as_str())
            .to_owned(),
    };
    let target = dispatched_target.as_str();
    let start_cursor = registry.current_task_cursor();
    let before = match &spawn_outcome {
        Some(outcome) => outcome.before.clone(),
        None => resolution
            .as_ref()
            .map(|resolved| resolved.before.clone())
            .or(guard_observation),
    }
    .or_else(|| agent_state_of(client, target));
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

    // A spawn dispatch submits its prompt with only what is left of the chain
    // the caller is waiting for; every other dispatch keeps the default budget.
    let prompt_budget = spawn_outcome.as_ref().map_or_else(
        || Duration::from_millis(DEFAULT_CALL_TIMEOUT_MS),
        |outcome| outcome.prompt_timeout,
    );
    let mut result = match client.call_with_timeout("agent.prompt", params, prompt_budget) {
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
    if let Some(resolved) = &resolution {
        result["requested_target"] = json!(requested_target);
        result["resolved_target"] = json!(resolved.target);
        result["candidates"] = json!(resolved.candidates);
    }

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

    // #332 B-2: when the submit cannot already prove the agent picked the
    // prompt up (`after` not `working`, no advanced seq), capture one bounded,
    // sanitized `pane.process_info` snapshot so the durable task can name the
    // foreground blocker instead of folding it into a vague boolean. Skipped
    // for spawn chains (a fresh pane cannot already be occupied) and whenever
    // the pane id is not resolvable. A failed probe yields `None`; it never
    // blocks task registration.
    let foreground_blocker = {
        let pane_id = after
            .as_ref()
            .and_then(|state| state.pane_id.as_deref())
            .or_else(|| before.as_ref().and_then(|state| state.pane_id.as_deref()));
        let agent_busy_before = before
            .as_ref()
            .is_some_and(|state| state.agent_status.as_deref() == Some("working"));
        let picked_up = after
            .as_ref()
            .is_some_and(|state| state.agent_status.as_deref() == Some("working"));
        let seq_advanced = before
            .as_ref()
            .and_then(|state| state.state_change_seq)
            .zip(after.as_ref().and_then(|state| state.state_change_seq))
            .is_some_and(|(before, after)| before != after);
        if submitted.is_some()
            && !picked_up
            && !seq_advanced
            && !agent_busy_before
            && spawn_outcome.is_none()
        {
            let agent_kind = after
                .as_ref()
                .and_then(|state| state.kind.as_deref())
                .or_else(|| before.as_ref().and_then(|state| state.kind.as_deref()));
            pane_id.and_then(|pane_id| {
                foreground_blocker_of(client, pane_id, registry.now_ms(), agent_kind)
            })
        } else {
            None
        }
    };
    match registry.register_task(RegisterTaskInput {
        task_id,
        target,
        requested_target,
        fingerprint: &fingerprint,
        idempotency_key,
        before: before.as_ref(),
        after: after.as_ref(),
        submitted,
        start_cursor,
        parent_target,
        parent_session_ref,
        stall_after_ms,
        max_runtime_ms,
        on_budget,
        request_id,
        foreground_blocker,
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

    // Spawn facts belong to the persisted dispatch outcome: a replay reports
    // the pane/agent this key actually created, including a failed prompt.
    if let Some(outcome) = &spawn_outcome {
        let facts = outcome.dispatch_facts(&result);
        let delivered = result.get("ok").and_then(Value::as_bool) == Some(true);
        if let Some(object) = result.as_object_mut() {
            object.insert("spawn".to_owned(), facts);
            if !delivered {
                object.insert("failed_step".to_owned(), json!("prompt"));
            }
        }
    }

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

    drop(role_admission);
    if let Some(wait) = wait.as_ref() {
        apply_task_wait(registry, &mut result, task_id, wait);
    }
    result
}

/// Reserve one dispatch key, or return the stored replay/rejection payload.
///
/// A replay is a pure observation: it resolves no evidence and submits nothing.
/// Outcome of reserving one dispatch idempotency key.
enum DispatchReservation {
    /// This call owns the key and must persist its outcome.
    Reserved(Option<String>),
    /// The key is already known: a completed outcome, a checkpoint, or an error.
    Known(Value),
}

fn reserve_dispatch(
    registry: &PromptRegistry,
    key: &str,
    fingerpr: &str,
    task_id: &str,
    wait: Option<&Value>,
) -> DispatchReservation {
    match registry.begin(key, fingerpr) {
        Ok(BeginPrompt::Reserved(op_id)) => DispatchReservation::Reserved(op_id),
        Ok(BeginPrompt::Replay(mut result)) => {
            insert_dispatch_id(&mut result, task_id);
            insert_task_id(&mut result, task_id);
            attach_task_observation(registry, &mut result, task_id, None);
            if let Some(wait) = wait {
                apply_task_wait(registry, &mut result, task_id, wait);
            }
            DispatchReservation::Known(result)
        }
        Err(mut error) => {
            insert_dispatch_id(&mut error, task_id);
            insert_task_id(&mut error, task_id);
            DispatchReservation::Known(error)
        }
    }
}

/// One validated optional `spawn` object of an `agent.task.dispatch` call.
///
/// Spawn turns the dispatch into one serial, single-confirmation operation:
/// create a task-owned pane, start the agent, wait for interactive readiness,
/// then run the unchanged dispatch path against that pane.
#[derive(Debug, Clone)]
struct SpawnRequest {
    kind: String,
    name: Option<String>,
    workspace: Option<String>,
    cwd: Option<String>,
    tab_label: Option<String>,
    split_from: Option<String>,
    direction: Option<String>,
    startup_timeout_ms: u64,
    /// Explicit model alias (1.2 S5). Part of the mutation identity: a
    /// different model is a different spawn intent under the same key.
    model: Option<String>,
    /// The validated, schema-deliverable form produced at parse time. `None`
    /// means either no model was requested or the live schema cannot carry
    /// one — the latter refuses during parse, before any pane is created.
    model_deliverable: Option<ModelDelivery>,
}

/// How a validated model alias is delivered on `agent.start`, decided during
/// `parse_spawn` so a schema that cannot carry a model fails before the chain
/// creates any pane (S5 review P1-3).
#[derive(Debug, Clone)]
enum ModelDelivery {
    /// The schema exposes a native `model` property: pass the canonical form.
    Property(String),
    /// pi's documented `--model` passthrough via the `args` array.
    Args(String),
}

impl SpawnRequest {
    /// Mutation identity for a spawn dispatch: the canonical spawn intent plus
    /// the text, never the pane the runtime is about to create. The startup
    /// budget is an execution preference like `wait`, so it cannot fork a key.
    fn request_fingerprint(&self, text: &str) -> String {
        let slot = |value: &Option<String>| value.clone().unwrap_or_default();
        let mut spec = format!(
            "spawn/1\nkind={}\nworkspace={}\ncwd={}\ntab_label={}\nsplit_from={}\ndirection={}\nname={}",
            self.kind,
            slot(&self.workspace),
            slot(&self.cwd),
            slot(&self.tab_label),
            slot(&self.split_from),
            slot(&self.direction),
            slot(&self.name),
        );
        // Keep the historical fingerprint byte-identical when no model is
        // requested, so stored keys from older runtimes still replay.
        if let Some(model) = &self.model {
            spec.push_str(&format!("\nmodel={model}"));
        }
        request_fingerprint(&spec, text)
    }
}

/// A spawn request with its live-snapshot facts resolved and no mutation done.
#[derive(Debug, Clone)]
struct SpawnDispatch {
    request: SpawnRequest,
    workspace_id: String,
    cwd: Option<PathBuf>,
    agent_name: String,
    tab_label: String,
}

/// A failure of exactly one spawn step, carrying the native error verbatim.
#[derive(Debug, Clone)]
struct SpawnStepError {
    code: String,
    message: String,
    hint: Option<String>,
}

impl SpawnStepError {
    fn native(error: HerdrError, hint: Option<&str>) -> Self {
        Self {
            code: error.code,
            message: error.message,
            hint: hint.map(str::to_owned),
        }
    }

    fn runtime(code: &str, message: String, hint: Option<&str>) -> Self {
        Self {
            code: code.to_owned(),
            message,
            hint: hint.map(str::to_owned),
        }
    }

    fn refusal(refusal: &Value, hint: Option<&str>) -> Self {
        Self {
            code: refusal
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("spawn_pane_refused")
                .to_owned(),
            message: refusal
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the pane could not be created")
                .to_owned(),
            hint: hint.map(str::to_owned),
        }
    }
}

/// Roll-forward facts for a spawn that may have failed or paused part-way.
#[derive(Default)]
struct SpawnProgress {
    pane_id: Option<String>,
    tab_id: Option<String>,
    agent_started: bool,
    steps: Vec<Value>,
    attempts: u64,
}

impl SpawnProgress {
    fn facts(&self, spawn: &SpawnDispatch) -> Value {
        let mut facts = json!({
            "pane_id": self.pane_id,
            "tab_id": self.tab_id,
            "agent_name": spawn.agent_name,
            "kind": spawn.request.kind,
            "workspace_id": spawn.workspace_id,
            "cwd": spawn.cwd.as_ref().map(|cwd| cwd.to_string_lossy().to_string()),
            "created": self.pane_id.is_some(),
            "agent_started": self.agent_started,
            "steps": self.steps,
        });
        // The admitted delivery form is durable checkpoint state: a resume
        // must deliver the same canonical model without re-running live
        // admission probes (S5 review P1-2).
        if let Some(object) = facts.as_object_mut() {
            if let Some(delivery) = &spawn.request.model_deliverable {
                let (form, canonical) = match delivery {
                    ModelDelivery::Property(canonical) => ("property", canonical.clone()),
                    ModelDelivery::Args(canonical) => ("args", canonical.clone()),
                };
                object.insert(
                    "model_deliverable".to_owned(),
                    json!({"form": form, "canonical": canonical}),
                );
            } else if let Some(model) = &spawn.request.model {
                object.insert("model".to_owned(), json!(model));
            }
        }
        facts
    }

    fn failure(&self, spawn: &SpawnDispatch, failed_step: &str, error: &SpawnStepError) -> Value {
        let mut failure = json!({
            "ok": false,
            "failed_step": failed_step,
            "code": error.code,
            "message": error.message,
            "delivery_state": "not_delivered",
            "submitted": false,
            "requires_human": false,
            "spawn": self.facts(spawn),
        });
        if let Some(hint) = &error.hint {
            failure["hint"] = json!(hint);
        }
        self.insert_reclaim(&mut failure);
        failure
    }

    /// A chain that ran out of the caller's budget is not a dead end: the pane
    /// and any started agent stay persisted under the same idempotency key, and
    /// replaying that key resumes exactly there instead of spawning again.
    fn checkpoint(
        &self,
        spawn: &SpawnDispatch,
        phase: &str,
        code: &str,
        message: String,
        hint: &str,
    ) -> Value {
        let mut checkpoint = json!({
            "ok": false,
            "failed_step": phase,
            "phase": phase,
            "resumable": true,
            "code": code,
            "message": message,
            "delivery_state": "not_delivered",
            "submitted": false,
            "requires_human": false,
            "attempts": self.attempts,
            "spawn": self.facts(spawn),
            "next": {
                "action": "replay_same_idempotency_key",
                "note": "replaying this key with the same spawn re-checks the persisted pane/agent, then prompts it once; it never creates a second pane, tab or agent",
                "raise": "spawn.startup_timeout_ms if the caller has a larger transport budget",
            },
            "hint": hint,
        });
        self.insert_reclaim(&mut checkpoint);
        checkpoint
    }

    fn insert_reclaim(&self, value: &mut Value) {
        if self.pane_id.is_some()
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "reclaim".to_owned(),
                json!({
                    "owner": "this_spawn",
                    "pane_id": self.pane_id,
                    "rule": "reclaim only this pane, and only when the agent is terminal or the pane is provably empty",
                }),
            );
        }
    }
}

/// A resumable checkpoint read back from the durable idempotency store.
struct SpawnResume {
    pane_id: String,
    tab_id: Option<String>,
    agent_started: bool,
    phase: String,
    steps: Vec<Value>,
    attempts: u64,
    /// The admitted delivery form persisted by `SpawnProgress::facts`: a
    /// resumed chain replays the same canonical model without re-validating.
    model_deliverable: Option<ModelDelivery>,
}

impl SpawnResume {
    fn progress(&self) -> SpawnProgress {
        SpawnProgress {
            pane_id: Some(self.pane_id.clone()),
            tab_id: self.tab_id.clone(),
            agent_started: self.agent_started,
            steps: self.steps.clone(),
            attempts: self.attempts,
        }
    }
}

/// Read one resumable spawn checkpoint out of a stored replay payload.
///
/// Only `agent_start`/`agent_ready` pauses resume: any other stored outcome is
/// a finished dispatch (or an uncertain prompt) and stays a pure replay.
fn spawn_checkpoint(result: &Value) -> Option<SpawnResume> {
    if result.get("resumable").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let phase = result
        .get("phase")
        .and_then(Value::as_str)
        .or_else(|| result.get("failed_step").and_then(Value::as_str))?;
    if phase != "agent_start" && phase != "agent_ready" {
        return None;
    }
    let spawn = result.get("spawn")?;
    let model_deliverable = spawn.get("model_deliverable").and_then(|value| {
        let canonical = value.get("canonical").and_then(Value::as_str)?.to_owned();
        match value.get("form").and_then(Value::as_str)? {
            "property" => Some(ModelDelivery::Property(canonical)),
            "args" => Some(ModelDelivery::Args(canonical)),
            _ => None,
        }
    });
    Some(SpawnResume {
        pane_id: spawn.get("pane_id").and_then(Value::as_str)?.to_owned(),
        tab_id: spawn
            .get("tab_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_started: spawn
            .get("agent_started")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        phase: phase.to_owned(),
        steps: spawn
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        attempts: result.get("attempts").and_then(Value::as_u64).unwrap_or(0),
        model_deliverable,
    })
}

/// A second concurrent replay of the same checkpoint must not prompt twice.
fn spawn_resume_in_progress(checkpoint: &SpawnResume) -> Value {
    json!({
        "ok": false,
        "failed_step": checkpoint.phase,
        "phase": checkpoint.phase,
        "resumable": true,
        "code": "spawn_resume_in_progress",
        "message": format!("another replay of this idempotency key is already resuming the spawn in pane {}", checkpoint.pane_id),
        "delivery_state": "not_delivered",
        "submitted": false,
        "requires_human": false,
        "hint": "wait for that replay to settle, then re-read the dispatch outcome instead of prompting again",
    })
}

/// Prompt headroom for one chain: a quarter of the chain, never the full 5s
/// reserve, so a small caller budget still leaves room for the pane and start.
fn spawn_prompt_reserve_ms(chain_ms: u64) -> u64 {
    SPAWN_PROMPT_RESERVE_MS.min(chain_ms / 4)
}

/// What a chain step may spend: the remaining chain minus the prompt reserve.
fn spawn_step_budget_ms(chain_ms: u64, remaining_ms: u64) -> u64 {
    remaining_ms.saturating_sub(spawn_prompt_reserve_ms(chain_ms))
}

/// A spawn whose pane, agent and readiness evidence all succeeded.
struct SpawnOutcome {
    pane_id: String,
    tab_id: Option<String>,
    agent_name: String,
    kind: String,
    workspace_id: String,
    cwd: Option<PathBuf>,
    steps: Vec<Value>,
    before: Option<AgentState>,
    /// Prompt submission budget: what is left of this attempt's chain.
    prompt_timeout: Duration,
    /// True when this attempt continued a persisted checkpoint.
    resumed: bool,
    attempts: u64,
}

impl SpawnOutcome {
    /// Public `spawn` facts plus the prompt step taken from the dispatch result.
    fn dispatch_facts(&self, result: &Value) -> Value {
        let mut steps = self.steps.clone();
        steps.push(json!({
            "step": "prompt",
            "method": "agent.prompt",
            "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "submitted": result.get("submitted").cloned().unwrap_or(Value::Null),
            "delivery_state": result.get("delivery_state").cloned().unwrap_or(Value::Null),
        }));
        json!({
            "pane_id": self.pane_id,
            "tab_id": self.tab_id,
            "agent_name": self.agent_name,
            "kind": self.kind,
            "workspace_id": self.workspace_id,
            "cwd": self.cwd.as_ref().map(|cwd| cwd.to_string_lossy().to_string()),
            "created": true,
            "resumed": self.resumed,
            "attempts": self.attempts,
            "steps": steps,
        })
    }
}

/// Validation rejection of a spawn dispatch: no pane, agent, or key exists yet.
fn spawn_invalid(message: &str) -> Value {
    json!({
        "ok": false,
        "failed_step": "validate",
        "code": "invalid_params",
        "message": message,
        "delivery_state": "not_delivered",
        "submitted": false,
        "requires_human": false,
    })
}

/// Parse and validate the optional `spawn` object without touching Herdr.
fn parse_spawn(args: &Value) -> Result<Option<SpawnRequest>, Value> {
    let Some(value) = args.get("spawn") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(spawn_invalid("spawn must be an object"));
    };
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "kind"
                | "name"
                | "workspace_id"
                | "cwd"
                | "tab_label"
                | "split_from"
                | "direction"
                | "startup_timeout_ms"
                | "model"
        ) {
            return Err(spawn_invalid(&format!(
                "spawn.{key} is not a supported spawn field"
            )));
        }
    }
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some(kind) if spawn_kind_is_valid(kind) => kind.to_owned(),
        Some(_) => {
            return Err(spawn_invalid(
                "spawn.kind must match [a-z][a-z0-9_-]{0,63} (as agent.start kind)",
            ));
        }
        None => return Err(spawn_invalid("spawn.kind is required")),
    };
    let name = match object.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if spawn_name_is_valid(name) => Some(name.clone()),
        Some(_) => {
            return Err(spawn_invalid("spawn.name must match [a-z][a-z0-9_-]{0,31}"));
        }
    };
    let workspace = spawn_optional_string(object, "workspace_id", 256)?;
    let cwd = spawn_optional_string(object, "cwd", 1024)?;
    let tab_label = spawn_optional_string(object, "tab_label", 64)?;
    let split_from = spawn_optional_string(object, "split_from", 256)?;
    let direction = match spawn_optional_string(object, "direction", 16)? {
        None => None,
        Some(direction) if direction == "right" || direction == "down" => Some(direction),
        Some(_) => return Err(spawn_invalid("spawn.direction must be right or down")),
    };
    if direction.is_some() && split_from.is_none() {
        return Err(spawn_invalid(
            "spawn.direction requires spawn.split_from: a new tab needs no split direction",
        ));
    }
    if workspace.is_none() && cwd.is_none() {
        return Err(spawn_invalid(
            "spawn requires workspace_id, or a cwd inside exactly one workspace",
        ));
    }
    let startup_timeout_ms = match object.get("startup_timeout_ms") {
        None | Some(Value::Null) => SPAWN_CHAIN_DEFAULT_MS,
        Some(value) => match value.as_u64() {
            Some(value) if (1..=SPAWN_CHAIN_MAX_MS).contains(&value) => value,
            _ => {
                return Err(spawn_invalid(
                    "spawn.startup_timeout_ms must be an integer in 1..=100000",
                ));
            }
        },
    };
    // 1.2 S5: the model alias is validated later, at admission time — after
    // the idempotency record has been consulted (S5 review P1-2). Parsing
    // here only preserves the raw alias so a replay whose key already owns a
    // durable result never touches the live model list or schema.
    let model = spawn_optional_string(object, "model", 256)?;
    Ok(Some(SpawnRequest {
        kind,
        name,
        workspace,
        cwd,
        tab_label,
        split_from,
        direction,
        startup_timeout_ms,
        model,
        model_deliverable: None,
    }))
}

/// Admit the spawn's explicit model alias against live evidence.
///
/// Runs only for a new reservation or a checkpoint resume whose agent step is
/// still pending — a replay of an already-recorded outcome never reaches this
/// function, so a transient pi/schema outage cannot downgrade a completed
/// dispatch to `model_alias_unknown` / `model_delivery_unsupported` (S5 review
/// P1-2). A refusal here is the key's recorded outcome: persisted like every
/// other pre-pane failure, so a same-key replay returns it instead of
/// re-running the live probes.
fn admit_spawn_model(spawn: &mut SpawnRequest) -> Result<(), Value> {
    let Some(alias) = &spawn.model else {
        return Ok(());
    };
    let check = crate::model_alias::validate_model_alias(&spawn.kind, alias);
    let crate::model_alias::ModelAliasCheck::Validated { canonical } = &check else {
        let mut refusal = crate::model_alias::model_alias_refusal(&spawn.kind, alias, &check);
        if let Some(object) = refusal.as_object_mut() {
            object.insert("failed_step".to_owned(), json!("validate"));
            object.insert("requires_human".to_owned(), json!(false));
        }
        return Err(refusal);
    };
    // The schema delivery decision is admission too: a schema that cannot
    // carry a model must refuse before the chain creates a pane, so the
    // caller is never left with a residual pane and a failed start.
    let deliverable = match crate::schema::agent_start_param_properties() {
        Ok(Some(properties)) if properties.contains_key("model") => {
            Some(ModelDelivery::Property(canonical.clone()))
        }
        Ok(Some(properties)) if properties.contains_key("args") && spawn.kind == "pi" => {
            Some(ModelDelivery::Args(canonical.clone()))
        }
        _ => None,
    };
    let Some(delivery) = deliverable else {
        let fact = crate::model_alias::validation_fact(&spawn.kind, alias, &check);
        return Err(json!({
            "ok": false,
            "code": "model_delivery_unsupported",
            "kind": spawn.kind,
            "requested_model": alias,
            "delivery_state": "not_delivered",
            "submitted": false,
            "fallback_allowed": false,
            "retryable": false,
            "model_validation": fact,
            "failed_step": "validate",
            "requires_human": false,
            "message": "the live herdr agent.start schema cannot carry a model (no model property, and no args passthrough for a pi kind); the validated alias was not silently dropped",
        }));
    };
    spawn.model_deliverable = Some(delivery);
    Ok(())
}

fn spawn_optional_string(
    object: &Map<String, Value>,
    field: &str,
    max_len: usize,
) -> Result<Option<String>, Value> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max_len || trimmed.chars().any(char::is_control) {
                return Err(spawn_invalid(&format!(
                    "spawn.{field} must be a non-empty string up to {max_len} bytes without control characters"
                )));
            }
            Ok(Some(trimmed.to_owned()))
        }
        Some(_) => Err(spawn_invalid(&format!("spawn.{field} must be a string"))),
    }
}

fn spawn_kind_is_valid(kind: &str) -> bool {
    let mut chars = kind.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && kind.len() <= 64
        && chars.all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || value == '_' || value == '-'
        })
}

fn spawn_name_is_valid(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && name.len() <= SPAWN_AGENT_NAME_MAX
        && chars.all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || value == '_' || value == '-'
        })
}

fn spawn_snapshot(registry: &PromptRegistry) -> Value {
    registry
        .inner
        .cache
        .as_ref()
        .map(|cache| cache.snapshot())
        .unwrap_or_else(|| json!({}))
}

fn spawn_workspaces(snapshot: &Value) -> Vec<&Value> {
    snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .map(|workspaces| workspaces.iter().collect())
        .unwrap_or_default()
}

fn spawn_workspace_id_of(workspace: &Value) -> Option<&str> {
    workspace
        .get("workspace_id")
        .and_then(Value::as_str)
        .or_else(|| workspace.get("id").and_then(Value::as_str))
}

fn spawn_workspace_root_of(workspace: &Value) -> Option<PathBuf> {
    let worktree = workspace.get("worktree")?.as_object()?;
    worktree
        .get("checkout_path")
        .and_then(Value::as_str)
        .or_else(|| worktree.get("path").and_then(Value::as_str))
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

/// A workspace created after the last snapshot is still addressable by id; a
/// label never is, so a typo cannot silently become a workspace id.
fn spawn_looks_like_workspace_id(value: &str) -> bool {
    value.len() <= 32
        && value.starts_with('w')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Resolve `spawn.workspace_id` (id or label) or, without it, the workspace
/// whose declared checkout path contains `spawn.cwd`.
fn resolve_spawn_workspace(snapshot: &Value, spawn: &SpawnRequest) -> Result<String, Value> {
    let workspaces = spawn_workspaces(snapshot);
    if let Some(wanted) = spawn.workspace.as_deref() {
        if workspaces
            .iter()
            .any(|workspace| spawn_workspace_id_of(workspace) == Some(wanted))
        {
            return Ok(wanted.to_owned());
        }
        let by_label = workspaces
            .iter()
            .copied()
            .filter(|workspace| workspace.get("label").and_then(Value::as_str) == Some(wanted))
            .collect::<Vec<_>>();
        match by_label.as_slice() {
            [workspace] => {
                if let Some(workspace_id) = spawn_workspace_id_of(workspace) {
                    return Ok(workspace_id.to_owned());
                }
            }
            [] => {
                if spawn_looks_like_workspace_id(wanted) {
                    return Ok(wanted.to_owned());
                }
                return Err(spawn_invalid(
                    "spawn.workspace_id did not match a live workspace id or label",
                ));
            }
            _ => {
                return Err(spawn_invalid(
                    "spawn.workspace_id matched more than one workspace label; pass the exact workspace id",
                ));
            }
        }
    }
    let Some(cwd) = spawn.cwd.as_deref() else {
        return Err(spawn_invalid(
            "spawn requires workspace_id, or a cwd inside exactly one workspace",
        ));
    };
    let cwd = Path::new(cwd);
    let matches = workspaces
        .iter()
        .filter(|workspace| {
            spawn_workspace_root_of(workspace).is_some_and(|root| cwd.starts_with(&root))
        })
        .filter_map(|workspace| spawn_workspace_id_of(workspace).map(str::to_owned))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [workspace_id] => Ok(workspace_id.clone()),
        [] => Err(spawn_invalid(
            "spawn.cwd is not inside any live workspace; pass spawn.workspace_id explicitly",
        )),
        _ => Err(spawn_invalid(
            "spawn.cwd is inside more than one workspace; pass spawn.workspace_id explicitly",
        )),
    }
}

/// Resolve and validate the spawn cwd. The check is lexical and component-wise
/// on purpose: a dispatch must never canonicalize a path or run `git`, so a
/// protected-folder TCC prompt can never appear inside a tool call.
fn resolve_spawn_cwd(
    snapshot: &Value,
    workspace_id: &str,
    spawn: &SpawnRequest,
) -> Result<Option<PathBuf>, Value> {
    let cwd = match spawn.cwd.as_deref() {
        Some(cwd) => PathBuf::from(cwd),
        None => spawn_workspaces(snapshot)
            .iter()
            .find(|workspace| spawn_workspace_id_of(workspace) == Some(workspace_id))
            .and_then(|workspace| spawn_workspace_root_of(workspace))
            .ok_or_else(|| {
                spawn_invalid(
                    "spawn.cwd is required: the workspace declares no checkout path to inherit",
                )
            })?,
    };
    if !cwd.is_absolute() {
        return Err(spawn_invalid("spawn.cwd must be an absolute path"));
    }
    if cwd
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(spawn_invalid("spawn.cwd must not contain '..' components"));
    }
    let roots = crate::fs_security::managed_roots(snapshot);
    if !roots.iter().any(|root| cwd.starts_with(root)) {
        let mut refusal =
            spawn_invalid("spawn.cwd must be inside a managed project root from the live topology");
        refusal["managed_roots"] = json!(
            roots
                .iter()
                .map(|root| root.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        );
        return Err(refusal);
    }
    Ok(Some(cwd))
}

/// Agent name for the new pane: the caller's name, or a stable kind+key name.
fn spawn_agent_name(spawn: &SpawnRequest, workspace_id: &str, idempotency_key: &str) -> String {
    if let Some(name) = &spawn.name {
        return name.clone();
    }
    let digest = stable_digest(&[
        b"herdr_prompt/spawn/name",
        spawn.kind.as_bytes(),
        workspace_id.as_bytes(),
        idempotency_key.as_bytes(),
    ]);
    let suffix = &digest[..8];
    let budget = SPAWN_AGENT_NAME_MAX.saturating_sub(suffix.len() + 1);
    let mut kind = spawn.kind.clone();
    kind.truncate(budget);
    while kind.ends_with(['-', '_']) {
        kind.pop();
    }
    format!("{kind}-{suffix}")
}

fn resolve_spawn_dispatch(
    registry: &PromptRegistry,
    spawn: &SpawnRequest,
    idempotency_key: &str,
) -> Result<SpawnDispatch, Value> {
    let snapshot = spawn_snapshot(registry);
    let workspace_id = resolve_spawn_workspace(&snapshot, spawn)?;
    let cwd = resolve_spawn_cwd(&snapshot, &workspace_id, spawn)?;
    let agent_name = spawn_agent_name(spawn, &workspace_id, idempotency_key);
    let tab_label = spawn
        .tab_label
        .clone()
        .unwrap_or_else(|| agent_name.clone());
    Ok(SpawnDispatch {
        request: spawn.clone(),
        workspace_id,
        cwd,
        agent_name,
        tab_label,
    })
}

struct SpawnPane {
    pane_id: String,
    tab_id: Option<String>,
    method: &'static str,
}

/// Create the task-owned pane: a new tab by default so the agent gets the full
/// tab width, `pane.split` only when the caller names its parent pane.
fn spawn_create_pane(
    client: &HerdrClient,
    registry: &PromptRegistry,
    spawn: &SpawnDispatch,
    timeout: Duration,
) -> Result<SpawnPane, SpawnStepError> {
    let cwd = spawn
        .cwd
        .as_ref()
        .map(|cwd| cwd.to_string_lossy().to_string());
    let Some(split_from) = spawn.request.split_from.as_deref() else {
        let mut params = Map::new();
        params.insert("workspace_id".to_owned(), json!(spawn.workspace_id));
        params.insert("label".to_owned(), json!(spawn.tab_label));
        params.insert("focus".to_owned(), json!(false));
        if let Some(cwd) = cwd {
            params.insert("cwd".to_owned(), json!(cwd));
        }
        let result = client
            .call_with_timeout("tab.create", Value::Object(params), timeout)
            .map_err(|error| {
                SpawnStepError::native(
                    error,
                    Some("verify with herdr_inspect before retrying: the tab may already exist"),
                )
            })?;
        let pane = result
            .get("root_pane")
            .or_else(|| result.get("pane"))
            .unwrap_or(&result);
        let pane_id = pane
            .get("pane_id")
            .and_then(Value::as_str)
            .or_else(|| result.get("pane_id").and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| {
                SpawnStepError::runtime(
                    "spawn_pane_unconfirmed",
                    "tab.create returned no pane id".to_owned(),
                    Some("the tab may already exist; inspect the workspace and reclaim it instead of spawning again"),
                )
            })?;
        let tab_id = result
            .get("tab")
            .and_then(|tab| tab.get("tab_id"))
            .or_else(|| result.get("tab_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Ok(SpawnPane {
            pane_id,
            tab_id,
            method: "tab.create",
        });
    };
    let mut params = json!({
        "target_pane_id": split_from,
        "direction": spawn.request.direction.as_deref().unwrap_or("right"),
        "focus": false,
    });
    if let Some(cwd) = cwd {
        params["cwd"] = json!(cwd);
    }
    // The same readable-width guard every other pane.split caller uses: a
    // narrow tab must fall back to a new tab instead of hosting an agent pane
    // whose excerpts are wrapping noise.
    let params = match crate::pane_width::guard_split(&spawn_snapshot(registry), &params) {
        crate::pane_width::SplitPlan::Forward(params) => params,
        crate::pane_width::SplitPlan::Refuse(refusal) => {
            return Err(SpawnStepError::refusal(
                &refusal,
                Some(
                    "drop spawn.split_from to open the agent in a new tab, which gets the full tab width",
                ),
            ));
        }
    };
    let result = client
        .call_with_timeout(crate::pane_width::SPLIT_METHOD, params, timeout)
        .map_err(|error| {
            SpawnStepError::native(
                error,
                Some("verify with herdr_inspect before retrying: the split may already exist"),
            )
        })?;
    let pane = result.get("pane").unwrap_or(&result);
    let pane_id = pane
        .get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| result.get("pane_id").and_then(Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| {
            SpawnStepError::runtime(
                "spawn_pane_unconfirmed",
                "pane.split returned no pane id".to_owned(),
                Some("the split may already exist; inspect the workspace and reclaim it instead of spawning again"),
            )
        })?;
    let tab_id = pane
        .get("tab_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(SpawnPane {
        pane_id,
        tab_id,
        method: crate::pane_width::SPLIT_METHOD,
    })
}

struct SpawnAgentReadiness {
    state: AgentState,
    ready: bool,
    /// The agent CLI's explicit readiness handshake; settles the idle heuristic.
    interactive_ready: bool,
}

/// Readiness evidence from one agent payload: `interactive_ready` is the
/// native signal, an `idle` lifecycle is the lifecycle equivalent.
fn spawn_agent_readiness(value: &Value) -> Option<SpawnAgentReadiness> {
    let state = agent_state_from_value(value)?;
    let agent = value.get("agent").unwrap_or(value);
    let interactive_ready = agent
        .get("interactive_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ready = interactive_ready || state.agent_status.as_deref() == Some("idle");
    Some(SpawnAgentReadiness {
        state,
        ready,
        interactive_ready,
    })
}

/// The shipped S4 ready criterion.
///
/// An agent is ready to receive a prompt when either
/// - `interactive_ready == true` (the agent CLI's own handshake), or
/// - `agent_status == "idle"` **and** that idle state has been observed for at
///   least [`SPAWN_READY_SETTLE_MS`] of the chain budget.
///
/// The settle window exists because idle is a self-reported status: a start that
/// is still building its composer can already claim idle. `first_idle_ms` is the
/// first observation of idle after the start; `None` means the handshake has not
/// happened and idle has not been observed yet.
fn spawn_readiness_satisfied(
    interactive_ready: bool,
    first_idle_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if interactive_ready {
        return true;
    }
    first_idle_ms.is_some_and(|first| now_ms.saturating_sub(first) >= SPAWN_READY_SETTLE_MS)
}

/// One bounded readiness probe. Returns the agent state only when it is ready.
fn spawn_probe_ready(client: &HerdrClient, pane_id: &str, timeout: Duration) -> Option<AgentState> {
    let value = client
        .call_with_timeout("agent.get", json!({"target": pane_id}), timeout)
        .ok()?;
    let readiness = spawn_agent_readiness(&value)?;
    readiness.ready.then_some(readiness.state)
}

enum SpawnReadyOutcome {
    Ready(AgentState),
    TimedOut(Option<String>),
    /// The started agent provably disappeared (1.2 S5 early-exit detection).
    /// Carries the native evidence code (`agent_not_found`/`unknown_pane`/…).
    Exited(String),
}

/// Native error codes that prove the agent/pane no longer exists, as opposed
/// to a transient probe failure. Shared with `prompt_failure`'s resolve check.
fn agent_gone_code(code: &str) -> bool {
    matches!(code, "agent_not_found" | "unknown_agent" | "unknown_pane")
}

/// Wait for interactive readiness strictly inside the chain deadline.
///
/// A resumed chain always gets one probe even below the poll floor: replaying a
/// checkpoint must be able to observe that the agent finished starting.
fn spawn_wait_ready(
    client: &HerdrClient,
    registry: &PromptRegistry,
    pane_id: &str,
    deadline_ms: u64,
    mut probe_even_when_exhausted: bool,
) -> SpawnReadyOutcome {
    let mut last_status = None;
    // A single gone-probe could race a pane teardown/recreate; two consecutive
    // gone probes are required before the chain declares the agent exited.
    let mut consecutive_gone = 0u32;
    // S4: first observation of the self-reported idle state. The handshake path
    // (`interactive_ready`) ignores it entirely.
    let mut first_idle_ms: Option<u64> = None;
    loop {
        let remaining = deadline_ms.saturating_sub(registry.now_ms());
        let probe_now = std::mem::take(&mut probe_even_when_exhausted);
        if !probe_now {
            if remaining < SPAWN_READY_MIN_POLL_MS {
                return SpawnReadyOutcome::TimedOut(last_status);
            }
            thread::sleep(SPAWN_READY_POLL.min(Duration::from_millis(remaining.max(1))));
        }
        let probe_timeout = SPAWN_READY_PROBE_TIMEOUT.min(Duration::from_millis(remaining.max(1)));
        let value = match client.call_with_timeout(
            "agent.get",
            json!({"target": pane_id}),
            probe_timeout,
        ) {
            Ok(value) => value,
            Err(error) => {
                if agent_gone_code(&error.code) {
                    consecutive_gone += 1;
                    if consecutive_gone >= 2 {
                        return SpawnReadyOutcome::Exited(error.code);
                    }
                } else {
                    consecutive_gone = 0;
                }
                continue;
            }
        };
        let Some(readiness) = spawn_agent_readiness(&value) else {
            // A successful probe with no agent payload at all is also evidence
            // the agent is gone (the pane fell back to a shell).
            consecutive_gone += 1;
            if consecutive_gone >= 2 {
                return SpawnReadyOutcome::Exited("agent_not_found".to_owned());
            }
            continue;
        };
        consecutive_gone = 0;
        if !readiness.ready {
            first_idle_ms = None;
            last_status = readiness.state.agent_status.clone();
            continue;
        }
        if readiness.interactive_ready {
            return SpawnReadyOutcome::Ready(readiness.state);
        }
        // Idle-only evidence: require the settle window before the prompt.
        let now = registry.now_ms();
        let first = *first_idle_ms.get_or_insert(now);
        if spawn_readiness_satisfied(false, Some(first), now) {
            return SpawnReadyOutcome::Ready(readiness.state);
        }
        last_status = readiness.state.agent_status.clone();
    }
}

/// Bounded, redacted pane tail for an `agent_start_failed` refusal. Reads the
/// pane directly (the agent is gone, so `agent.read` may not serve it) and
/// never returns raw secrets: ANSI/control stripping plus credential redaction.
fn spawn_failure_pane_tail(client: &HerdrClient, pane_id: &str) -> Option<String> {
    let response = client
        .call_with_timeout(
            "pane.read",
            json!({"pane_id": pane_id, "lines": SPAWN_FAILURE_TAIL_LINES}),
            Duration::from_secs(2),
        )
        .ok()?;
    let read = response.get("read").unwrap_or(&response);
    let text = ["content", "text", "output"]
        .into_iter()
        .find_map(|key| read.get(key).and_then(Value::as_str))?;
    let stripped = strip_ansi_and_control(text);
    let redacted = crate::work_capture::redact(&stripped);
    let tail = redacted
        .lines()
        .rev()
        .take(SPAWN_FAILURE_TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// The early-exit refusal: the agent was started but provably exited before
/// the prompt was submitted. Not resumable — replaying must not prompt a dead
/// pane; the caller reclaims it and dispatches a fresh spawn.
fn spawn_agent_start_failed(
    client: &HerdrClient,
    progress: &SpawnProgress,
    spawn: &SpawnDispatch,
    pane_id: &str,
    evidence_code: &str,
) -> Value {
    let mut failure = progress.failure(
        spawn,
        "agent_start",
        &SpawnStepError {
            code: "agent_start_failed".to_owned(),
            message: format!(
                "agent in pane {pane_id} exited before the prompt was submitted (evidence: {evidence_code})"
            ),
            hint: Some(
                "the agent died on start (often an invalid model/alias or missing credentials); inspect the pane tail, reclaim this pane, and dispatch a new spawn with a corrected model".to_owned(),
            ),
        },
    );
    if let Some(tail) = spawn_failure_pane_tail(client, pane_id) {
        failure["pane_tail"] = json!(tail);
        failure["pane_tail_redacted"] = json!(true);
    }
    failure
}

/// The early-exit watch window, configurable via
/// `HERDR_MCP_SPAWN_EARLY_EXIT_MS` (0 disables the post-readiness watch;
/// the readiness loop still detects an agent that dies before ready).
/// Defaults to ~10s and is always capped by the chain budget.
fn spawn_early_exit_window_ms() -> u64 {
    std::env::var("HERDR_MCP_SPAWN_EARLY_EXIT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(SPAWN_EARLY_EXIT_WINDOW_MS))
        .unwrap_or(SPAWN_EARLY_EXIT_WINDOW_MS)
}

/// Post-readiness early-exit watch (1.2 S5). After the start proved ready,
/// keep observing for a bounded window so a dead-on-start agent is reported
/// as `agent_start_failed` instead of a delivered prompt to a corpse. The
/// window is capped by the chain deadline minus the prompt reserve AND a
/// fraction of the chain, so a small caller budget keeps its prompt reserve;
/// a healthy agent that stays present ends the watch at the window.
fn spawn_watch_early_exit(
    client: &HerdrClient,
    registry: &PromptRegistry,
    pane_id: &str,
    chain_deadline_ms: u64,
    chain_ms: u64,
) -> Option<String> {
    if registry
        .inner
        .spawn_early_exit_watch_disabled
        .load(Ordering::SeqCst)
    {
        return None;
    }
    let window = spawn_early_exit_window_ms();
    if window == 0 {
        return None;
    }
    let reserve = spawn_prompt_reserve_ms(chain_ms);
    let watch_deadline = registry
        .now_ms()
        .saturating_add(window.min(chain_ms / 4))
        .min(chain_deadline_ms.saturating_sub(reserve));
    let mut consecutive_gone = 0u32;
    loop {
        let now = registry.now_ms();
        if now >= watch_deadline {
            return None;
        }
        thread::sleep(SPAWN_EARLY_EXIT_POLL.min(Duration::from_millis(
            watch_deadline.saturating_sub(now).max(1),
        )));
        // Cap each watch probe well below the fake-server read window and the
        // remaining watch budget so a slow peer cannot stretch the window.
        let probe_timeout = SPAWN_READY_PROBE_TIMEOUT.min(Duration::from_millis(
            watch_deadline
                .saturating_sub(registry.now_ms())
                .clamp(1, 1_000),
        ));
        match client.call_with_timeout("agent.get", json!({"target": pane_id}), probe_timeout) {
            Ok(value) => {
                if spawn_agent_readiness(&value).is_some() {
                    consecutive_gone = 0;
                } else {
                    consecutive_gone += 1;
                    if consecutive_gone >= 2 {
                        return Some("agent_not_found".to_owned());
                    }
                }
            }
            Err(error) => {
                if agent_gone_code(&error.code) {
                    consecutive_gone += 1;
                    if consecutive_gone >= 2 {
                        return Some(error.code);
                    }
                } else {
                    consecutive_gone = 0;
                }
            }
        }
    }
}

/// Create (or resume) the pane, start the agent, and prove interactive
/// readiness, spending only what is left of one caller-visible chain budget.
fn spawn_chain(
    client: &HerdrClient,
    registry: &PromptRegistry,
    spawn: &SpawnDispatch,
    resume: Option<&SpawnResume>,
) -> Result<SpawnOutcome, Value> {
    let chain_ms = spawn.request.startup_timeout_ms.min(SPAWN_CHAIN_MAX_MS);
    let chain_started = registry.now_ms();
    let deadline = chain_started.saturating_add(chain_ms);
    let resumed = resume.is_some();
    let mut progress = match resume {
        Some(checkpoint) => checkpoint.progress(),
        None => SpawnProgress::default(),
    };
    if let Some(checkpoint) = resume {
        progress.attempts = progress.attempts.saturating_add(1);
        progress.steps.push(json!({
            "step": "resume",
            "method": "agent.task.dispatch",
            "ok": true,
            "phase": checkpoint.phase,
            "attempt": progress.attempts,
            "pane_id": checkpoint.pane_id,
        }));
    }

    // 1. Pane. A resumed chain reuses the persisted pane and never creates one.
    if progress.pane_id.is_none() {
        let step_at = registry.now_ms();
        let remaining = deadline.saturating_sub(step_at);
        if remaining == 0 {
            return Err(progress.checkpoint(
                spawn,
                "pane_create",
                "spawn_budget_exhausted",
                "the spawn chain had no budget left for creating the pane".to_owned(),
                "replay the same idempotency key with a larger spawn.startup_timeout_ms",
            ));
        }
        // Socket floor: `remaining` is the chain's accounting unit, but a
        // budget measured in single-digit ms must not starve the pane call of
        // any socket window — the step gate above already decided the chain
        // may spend one mutation, and the next gate still re-checks the
        // deadline before the agent step.
        let timeout = Duration::from_millis(
            remaining.clamp(SPAWN_PANE_SOCKET_FLOOR_MS, SPAWN_PANE_TIMEOUT_MS),
        );
        let created = match spawn_create_pane(client, registry, spawn, timeout) {
            Ok(created) => created,
            Err(error) => return Err(progress.failure(spawn, "pane_create", &error)),
        };
        progress.pane_id = Some(created.pane_id.clone());
        progress.tab_id = created.tab_id.clone();
        progress.steps.push(json!({
            "step": "pane_create",
            "method": created.method,
            "ok": true,
            "pane_id": created.pane_id,
            "tab_id": created.tab_id,
            "duration_ms": registry.now_ms().saturating_sub(step_at),
        }));
    }
    let pane_id = progress
        .pane_id
        .clone()
        .expect("a spawn chain always has a pane before the agent step");

    // 2. Agent. Every step gets only the remaining budget: the prompt reserve
    //    is subtracted here, never added on top of the caller's window.
    let mut before = None;
    let mut started_method = "agent.start";
    if !progress.agent_started {
        // A resumed chain first checks whether the agent already came up, so a
        // pane whose start completed while paused is reused, not restarted.
        if resumed {
            let probe = Duration::from_millis(
                deadline
                    .saturating_sub(registry.now_ms())
                    .clamp(1, SPAWN_READY_PROBE_TIMEOUT.as_millis() as u64),
            );
            if let Some(state) = spawn_probe_ready(client, &pane_id, probe) {
                started_method = "agent.get";
                progress.agent_started = true;
                before = Some(state);
                progress.steps.push(json!({
                    "step": "agent_start",
                    "method": "agent.get",
                    "ok": true,
                    "already_started": true,
                    "pane_id": pane_id,
                }));
            }
        }
        if !progress.agent_started {
            let step_at = registry.now_ms();
            let remaining = deadline.saturating_sub(step_at);
            let start_budget = spawn_step_budget_ms(chain_ms, remaining);
            if start_budget < SPAWN_NATIVE_START_FLOOR_MS {
                return Err(progress.checkpoint(
                    spawn,
                    "agent_start",
                    "spawn_budget_exhausted",
                    format!(
                        "only {remaining}ms of the spawn chain remained, which cannot start an agent (native floor {SPAWN_NATIVE_START_FLOOR_MS}ms plus the prompt reserve)"
                    ),
                    "replay the same idempotency key with a larger spawn.startup_timeout_ms; the pane is reused and the agent is started at most once",
                ));
            }
            let native_timeout = start_budget.min(SPAWN_CHAIN_MAX_MS);
            let mut start_params = json!({
                "name": spawn.agent_name,
                "kind": spawn.request.kind,
                "pane_id": pane_id,
                "timeout_ms": native_timeout,
            });
            // 1.2 S5: the alias was validated and its delivery form chosen in
            // parse_spawn; here we only apply it. A schema that cannot carry a
            // model refused during parse, before this pane ever existed.
            match &spawn.request.model_deliverable {
                Some(ModelDelivery::Property(canonical)) => {
                    start_params["model"] = json!(canonical);
                }
                Some(ModelDelivery::Args(canonical)) => {
                    start_params["args"] = json!(["--model", canonical]);
                }
                None => {}
            }
            match client.call_with_timeout(
                "agent.start",
                start_params,
                Duration::from_millis(start_budget),
            ) {
                Ok(started) => {
                    progress.agent_started = true;
                    progress.steps.push(json!({
                        "step": "agent_start",
                        "method": "agent.start",
                        "ok": true,
                        "agent_name": spawn.agent_name,
                        "kind": spawn.request.kind,
                        "duration_ms": registry.now_ms().saturating_sub(step_at),
                    }));
                    if let Some(readiness) = spawn_agent_readiness(&started)
                        && readiness.ready
                    {
                        before = Some(readiness.state);
                    }
                }
                Err(error) if error.code == "timeout" => {
                    return Err(progress.checkpoint(
                        spawn,
                        "agent_start",
                        "spawn_budget_exhausted",
                        format!(
                            "agent.start did not confirm within {start_budget}ms of the spawn chain"
                        ),
                        "the pane may already contain a starting agent; replay the same idempotency key to re-check readiness and prompt once instead of spawning again",
                    ));
                }
                Err(error) => {
                    return Err(progress.failure(
                        spawn,
                        "agent_start",
                        &SpawnStepError::native(
                            error,
                            Some("the new pane may already contain a started agent; verify it with herdr_inspect and reclaim it instead of spawning again"),
                        ),
                    ));
                }
            }
        }
    }

    // 3. Readiness, strictly inside what is left of the caller's budget.
    let before = match before {
        Some(state) => state,
        None => {
            let step_at = registry.now_ms();
            match spawn_wait_ready(client, registry, &pane_id, deadline, resumed) {
                SpawnReadyOutcome::Ready(state) => {
                    progress.steps.push(json!({
                        "step": "agent_ready",
                        "method": "agent.get",
                        "ok": true,
                        "agent_status": state.agent_status,
                        "duration_ms": registry.now_ms().saturating_sub(step_at),
                    }));
                    state
                }
                SpawnReadyOutcome::TimedOut(last_status) => {
                    // S4: a request that never reached delivery is
                    // `not_delivered` (see `checkpoint`), never
                    // `delivery_uncertain`; the public code names the cause.
                    return Err(progress.checkpoint(
                        spawn,
                        "agent_ready",
                        "agent_not_ready",
                        format!(
                            "agent in pane {pane_id} was not interactive-ready within the {}ms spawn chain (last status: {})",
                            chain_ms,
                            last_status.as_deref().unwrap_or("unknown")
                        ),
                        "replay the same idempotency key to re-check readiness and prompt once; the persisted pane and agent are reused",
                    ));
                }
                SpawnReadyOutcome::Exited(evidence) => {
                    return Err(spawn_agent_start_failed(
                        client, &progress, spawn, &pane_id, &evidence,
                    ));
                }
            }
        }
    };
    if started_method == "agent.start" && before.agent_status.as_deref() == Some("idle") {
        // The start response itself proved readiness; record that evidence.
        progress.steps.push(json!({
            "step": "agent_ready",
            "method": "agent.start",
            "ok": true,
            "agent_status": before.agent_status,
        }));
    }

    // 4. Early-exit watch (1.2 S5): a freshly started agent that dies within
    //    the bounded window is reported as agent_start_failed, never prompted.
    if started_method == "agent.start"
        && let Some(evidence) =
            spawn_watch_early_exit(client, registry, &pane_id, deadline, chain_ms)
    {
        return Err(spawn_agent_start_failed(
            client, &progress, spawn, &pane_id, &evidence,
        ));
    }

    let remaining = deadline.saturating_sub(registry.now_ms());
    Ok(SpawnOutcome {
        pane_id,
        tab_id: progress.tab_id.clone(),
        agent_name: spawn.agent_name.clone(),
        kind: spawn.request.kind.clone(),
        workspace_id: spawn.workspace_id.clone(),
        cwd: spawn.cwd.clone(),
        steps: progress.steps,
        before: Some(before),
        // The prompt must land inside the same chain the caller waits for.
        prompt_timeout: Duration::from_millis(remaining.max(1)),
        resumed,
        attempts: progress.attempts,
    })
}

/// Persist the spawn outcome for this key and annotate the identities, so the
/// replay of a partially failed spawn never repeats a mutation.
fn finish_spawn_failure(
    registry: &PromptRegistry,
    idempotency_key: Option<&str>,
    fingerprint: &str,
    dispatch_id: &str,
    mut failure: Value,
) -> Value {
    insert_dispatch_id(&mut failure, dispatch_id);
    insert_task_id(&mut failure, dispatch_id);
    if let Some(key) = idempotency_key
        && let Err(persist_error) = registry.complete(key, fingerprint, &failure)
    {
        return annotate_idempotency_persistence(failure, persist_error);
    }
    failure
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
                "one_shot": {
                    "method": AGENT_TASK_DISPATCH_METHOD,
                    "params": {
                        "spawn": {"kind": "<agent kind>", "workspace_id": "<workspace id or label>", "cwd": "<optional project cwd>"},
                        "text": "<same task card>",
                        "idempotency_key": "<new key>"
                    },
                    "note": "one herdr_call creates the pane, starts the agent, waits for readiness and prompts it with a single confirmation",
                },
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
                "agent target did not resolve and nothing was submitted. This is a pre-dispatch condition, not a human boundary. For a NEW agent, dispatch once with herdr_mcp.agent.task.dispatch and the spawn object (kind/workspace_id/cwd) instead of inspect -> split -> agent.start -> dispatch; otherwise re-read herdr_inspect, select a compatible startable Agent, and use a verified task-owned free shell pane or pane.split a task-owned pane before agent.start. Re-prompt with the same idempotency_key after the Agent is ready; never infer pane ownership from agent:null."
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
/// #332 B-2: one bounded `pane.process_info` observation that lets
/// `agent.task.status` answer "what is holding the pane" instead of folding a
/// blocked prompt into a vague boolean. Returns `None` when the probe cannot
/// run or the foreground is shell-only (the pane is not blocked).
///
/// The summary is deliberately non-secret: process `name` values and, when the
/// runtime reports one, a strict-allowlist `command_prefix`. argv values are
/// never copied through; any token containing `=`, quoting, punctuation that
/// could carry a value, or a flag whose name mentions a credential drops the
/// rest of the prefix. No environment variables are ever consulted.
fn foreground_blocker_of(
    client: &HerdrClient,
    pane_id: &str,
    observed_at_ms: u64,
    agent_kind: Option<&str>,
) -> Option<Value> {
    let info = client
        .call_with_timeout(
            "pane.process_info",
            json!({"pane_id": pane_id}),
            FOREGROUND_PROBE_TIMEOUT,
        )
        .ok()?;
    let info = info.get("process_info").unwrap_or(&info);
    let shell_pid = finite_process_id(info.get("shell_pid"));
    let foreground = info
        .get("foreground_processes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let processes: Vec<Value> = foreground
        .iter()
        .filter_map(|process| sanitize_foreground_process(process, shell_pid, agent_kind))
        .take(FOREGROUND_BLOCKER_MAX_PROCESSES)
        .collect();
    let shell_owns_foreground = shell_pid.is_some()
        && finite_process_id(info.get("foreground_process_group_id")) == shell_pid;
    let is_own = |process: &Value| {
        finite_process_id(process.get("pid")) == shell_pid
            || process.get("agent").and_then(Value::as_bool) == Some(true)
    };
    let shell_only = shell_owns_foreground
        && foreground
            .iter()
            .all(|process| finite_process_id(process.get("pid")) == shell_pid);
    if processes.is_empty() || shell_only || processes.iter().all(is_own) {
        return None;
    }
    let non_shell_count = processes
        .iter()
        .filter(|process| {
            process.get("shell").and_then(Value::as_bool) != Some(true)
                && process.get("agent").and_then(Value::as_bool) != Some(true)
        })
        .count();
    Some(json!({
        "pane_id": pane_id,
        "observed_at_ms": observed_at_ms,
        "source": "pane.process_info",
        "shell_owns_foreground": shell_owns_foreground,
        "non_shell_processes": non_shell_count,
        "processes": processes,
        // Deliberately no `command`/`argv`/`env` echo: names only, plus the
        // allowlisted `command_prefix` per process when the runtime reports it.
    }))
}

fn finite_process_id(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .and_then(Value::as_i64)
                .and_then(|value| u64::try_from(value).ok())
        })
        .filter(|value| *value > 0)
}

/// Keeps a process name usable as a classifier: basename characters, no
/// whitespace, no control bytes, hard-truncated.
fn sanitize_process_name(raw: &str) -> String {
    let tail = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    tail.chars()
        .filter(|ch| !ch.is_control() && !ch.is_whitespace())
        .take(FOREGROUND_NAME_MAX_CHARS)
        .collect()
}

/// One allowlisted token for a command prefix: bare alnum plus `-_.`; no
/// `=`, no quoting, no path separators, so no argument value can ride along.
fn sanitize_command_token(raw: &str) -> Option<String> {
    let token: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect();
    (!token.is_empty()).then_some(token)
}

/// Builds a non-secret command prefix: basename of argv[0] plus at most
/// `FOREGROUND_COMMAND_MAX_TOKENS` allowlisted words. A `-`-prefixed flag
/// containing a credential word, a bare credential marker (`bearer`,
/// `apikey`...), or any token containing `=` terminates the prefix rather
/// than leaking a value.
fn sanitize_command_prefix(raw: &str) -> Option<String> {
    const CREDENTIAL_WORDS: &[&str] = &[
        "token",
        "secret",
        "password",
        "passwd",
        "key",
        "auth",
        "credential",
        "bearer",
        "apikey",
        "api_key",
        "cookie",
    ];
    let mut out = sanitize_process_name(raw.split_whitespace().next()?);
    if out.is_empty() {
        return None;
    }
    for token in raw
        .split_whitespace()
        .skip(1)
        .take(FOREGROUND_COMMAND_MAX_TOKENS - 1)
    {
        let lower = token.to_ascii_lowercase();
        // A `-`-prefixed flag containing a credential word, or a bare token
        // that is exactly a credential marker (`bearer`, `apikey`...): either
        // ends the prefix before the following value can leak.
        let credential_flag = if lower.starts_with('-') {
            CREDENTIAL_WORDS.iter().any(|word| lower.contains(word))
        } else {
            CREDENTIAL_WORDS.iter().any(|word| lower == *word)
        };
        if credential_flag || token.contains('=') {
            break;
        }
        let Some(word) = sanitize_command_token(token) else {
            break;
        };
        out.push(' ');
        out.push_str(&word);
    }
    if out.chars().count() > FOREGROUND_COMMAND_MAX_CHARS {
        let truncated: String = out.chars().take(FOREGROUND_COMMAND_MAX_CHARS).collect();
        return Some(format!("{truncated}…"));
    }
    Some(out)
}

/// A foreground process counts as the agent itself only when the reported
/// kind matches its process-name basename, so `less`/`vim` on an `omp` pane
/// still surface as blockers while the agent's own foreground process does
/// not. Case-insensitive; never treats an unmatched kind as agent-owned.
fn agent_process_names_match(kind: &str, process_name: &str) -> bool {
    !kind.is_empty() && process_name.eq_ignore_ascii_case(kind)
}

/// Shapes one `foreground_processes` entry for the durable record: pid, a
/// sanitized name, a `shell` flag when it owns the pane's shell pid, and the
/// allowlisted command prefix when the runtime reports `command`/`argv`.
fn sanitize_foreground_process(
    process: &Value,
    shell_pid: Option<u64>,
    agent_kind: Option<&str>,
) -> Option<Value> {
    let pid = finite_process_id(process.get("pid"));
    let name = process
        .get("name")
        .and_then(Value::as_str)
        .map(sanitize_process_name)
        .filter(|name| !name.is_empty());
    let is_agent = agent_kind.is_some_and(|kind| {
        name.as_deref()
            .is_some_and(|name| agent_process_names_match(kind, name))
    });
    if pid.is_none() && name.is_none() {
        return None;
    }
    let mut entry = Map::new();
    if let Some(pid) = pid {
        entry.insert("pid".to_owned(), json!(pid));
    }
    if let Some(name) = name {
        entry.insert("name".to_owned(), json!(name));
    }
    if shell_pid.is_some() && pid == shell_pid {
        entry.insert("shell".to_owned(), json!(true));
    }
    // #332 B-2: the agent's own process is occupancy, not a blocker; the flag
    // keeps it readable in the summary while excluding it from the count.
    if is_agent {
        entry.insert("agent".to_owned(), json!(true));
    }
    let command = process
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            process.get("argv").and_then(Value::as_array).map(|argv| {
                argv.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        });
    if let Some(prefix) = command.as_deref().and_then(sanitize_command_prefix) {
        entry.insert("command_prefix".to_owned(), json!(prefix));
    }
    Some(Value::Object(entry))
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
        kind: agent.get("kind").and_then(Value::as_str).map(str::to_owned),
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
        // S4: one receipt per dispatch, including a spawn chain whose failure
        // still owns exactly one durable record. `receipt_id` aliases the task
        // id (S3 vocabulary) and is the id the caller waits on.
        object
            .entry("receipt_id".to_owned())
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
        kind: after.get("kind").and_then(Value::as_str).map(str::to_owned),
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
            kind: None,
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
            kind: None,
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
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                if request["method"] == "agent.prompt" {
                    prompt_texts
                        .lock()
                        .unwrap()
                        .push(request["params"]["text"].as_str().unwrap().to_owned());
                }
                response["id"] = request["id"].clone();
                if !write_fake_reply(&mut stream, &response.to_string()) {
                    break;
                }
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

    fn role_registry(agents: Value) -> PromptRegistry {
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let mut registry = PromptRegistry::with_store(store);
        Arc::get_mut(&mut registry.inner).unwrap().cache = Some(Arc::new(
            EventCache::from_snapshot_for_test(json!({"agents": agents, "panes": []})),
        ));
        registry
    }

    fn role_submitted(pane: &str) -> Value {
        json!({"result": {"prompt": {"status": "submitted", "agent": {
            "pane_id": pane, "agent_status": "working", "state_change_seq": 2,
        }}}})
    }

    fn spawn_project_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "herdr-mcp-spawn-{}-{}-{label}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn spawn_snapshot(root: &Path, pane_columns: u16) -> Value {
        json!({
            "workspaces": [{
                "workspace_id": "w1",
                "label": "demo-project",
                "worktree": {"checkout_path": root.to_string_lossy()},
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy(),
                "agent_status": "idle",
            }],
            "agents": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "kind": "codex",
                "agent_status": "idle",
            }],
            "layouts": [{
                "workspace_id": "w1",
                "focused_pane_id": "w1:p1",
                "zoomed": false,
                "panes": [{
                    "pane_id": "w1:p1",
                    "focused": true,
                    "rect": {"x": 0, "y": 0, "width": pane_columns, "height": 30},
                }],
            }],
        })
    }

    /// Disables the S5 post-readiness early-exit watch for the lifetime of the
    /// guard, restoring the previous value on drop. Scripted spawn fakes answer
    /// a fixed request sequence and must not receive extra watch probes. The
    /// guard owns the crate-wide env lock for its whole lifetime, so callers
    /// must never hold `test_env::lock()` across a `spawn_registry*` call: a
    /// second acquire on the same non-reentrant mutex deadlocks the test.
    /// The guard is returned alongside the registry so it lives exactly as
    /// long as the test's registry binding, never leaking into a concurrent
    /// test.
    struct SpawnWatchOff {
        previous: Option<std::ffi::OsString>,
        // Held for the entire watch-off window; named `_lock` only to keep the
        // field alive, the drop order restores env before releasing it.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for SpawnWatchOff {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => env::set_var("HERDR_MCP_SPAWN_EARLY_EXIT_MS", value),
                    None => env::remove_var("HERDR_MCP_SPAWN_EARLY_EXIT_MS"),
                }
            }
        }
    }

    fn spawn_registry_with(
        store: Arc<Mutex<StateStore>>,
        root: &Path,
        pane_columns: u16,
    ) -> (PromptRegistry, SpawnWatchOff) {
        // Lock first, then read/write env: a test needing the default watch
        // window holds the same lock so its window stays uncontaminated.
        let lock = crate::test_env::lock();
        let guard = SpawnWatchOff {
            previous: env::var_os("HERDR_MCP_SPAWN_EARLY_EXIT_MS"),
            _lock: lock,
        };
        unsafe { env::set_var("HERDR_MCP_SPAWN_EARLY_EXIT_MS", "0") };
        let mut registry = PromptRegistry::with_store(store);
        Arc::get_mut(&mut registry.inner).unwrap().cache = Some(Arc::new(
            EventCache::from_snapshot_for_test(spawn_snapshot(root, pane_columns)),
        ));
        (registry, guard)
    }

    fn spawn_registry(root: &Path, pane_columns: u16) -> (PromptRegistry, SpawnWatchOff) {
        spawn_registry_with(
            Arc::new(Mutex::new(StateStore::open(":memory:").unwrap())),
            root,
            pane_columns,
        )
    }

    /// Whether an error means the client on the other end of a fake socket is
    /// already gone. A Runtime under test with a deliberately short timeout can
    /// close the connection before the scripted reply lands; on a loaded CI
    /// runner that is a legitimate outcome of the code under test, never a
    /// harness failure, so a fake must stop instead of panicking the test
    /// thread (the assertions still verify what the Runtime actually did).
    fn fake_peer_gone(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof
        )
    }

    /// Write one scripted reply to a fake socket.
    ///
    /// Returns `false` when the peer already hung up so the caller can stop the
    /// fake loop; any other write error is still a harness bug and panics.
    fn write_fake_reply<W: std::io::Write>(stream: &mut W, payload: &str) -> bool {
        match writeln!(stream, "{payload}") {
            Ok(()) => true,
            Err(error) if fake_peer_gone(&error) => false,
            Err(error) => panic!("fake Herdr socket could not write a reply: {error}"),
        }
    }

    /// Socket fake that records every request so a test can prove which native
    /// calls happened (and, for a replay, that none happened again).
    fn scripted_spawn_server(
        listener: UnixListener,
        responses: Vec<Value>,
        requests: Arc<Mutex<Vec<Value>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for mut response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                requests.lock().unwrap().push(request.clone());
                response["id"] = request["id"].clone();
                if !write_fake_reply(&mut stream, &response.to_string()) {
                    break;
                }
            }
        })
    }

    /// Fake Herdr socket that answers any number of requests until stopped, so
    /// a bounded readiness poll loop does not need one scripted reply per poll.
    fn repeating_spawn_server(
        listener: UnixListener,
        stop: Arc<std::sync::atomic::AtomicBool>,
        responses: Vec<(&'static str, Value)>,
        requests: Arc<Mutex<Vec<Value>>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    Err(_) => break,
                };
                // Accepted streams can inherit the listener's nonblocking mode on macOS.
                stream.set_nonblocking(false).unwrap();
                // A peer that already closed can make this fail on macOS.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => continue,
                    Ok(_) => {}
                    // A bounded client probe can close before the fake reads;
                    // that is the code under test, not a harness failure.
                    Err(error) if fake_peer_gone(&error) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                requests.lock().unwrap().push(request.clone());
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                let response = responses
                    .iter()
                    .find(|(name, _)| *name == method)
                    .map(|(_, response)| response.clone())
                    .unwrap_or_else(
                        || json!({"error": {"code": "unscripted_method", "message": method}}),
                    );
                let envelope = if let Some(error) = response.get("error") {
                    json!({"id": request["id"], "error": error})
                } else {
                    json!({"id": request["id"], "result": response})
                };
                if !write_fake_reply(&mut stream, &envelope.to_string()) {
                    continue;
                }
            }
        })
    }

    fn spawn_requested_methods(requests: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|request| request["method"].as_str().map(str::to_owned))
            .collect()
    }

    fn spawn_tab_created() -> Value {
        json!({"result": {
            "type": "tab_created",
            "tab": {"tab_id": "w1:t2", "workspace_id": "w1", "label": "feat-x"},
            "root_pane": {"pane_id": "w1:p9", "tab_id": "w1:t2", "workspace_id": "w1"},
        }})
    }

    /// S4: seed one durable task row with a chosen lifecycle, update time and
    /// retention deadline. `expires_at` in the past is what the archive view
    /// reads; the row itself is never deleted.
    #[allow(clippy::too_many_arguments)]
    fn seed_task_row(
        registry: &PromptRegistry,
        store: &Arc<Mutex<StateStore>>,
        task_id: &str,
        target: &str,
        parent_session_ref: Option<&str>,
        lifecycle: PromptTaskLifecycle,
        updated_at_ms: u64,
        expires_at: Option<i64>,
    ) {
        let mut task = registry
            .register_task(RegisterTaskInput {
                task_id,
                target,
                requested_target: None,
                fingerprint: "fp-s4",
                idempotency_key: None,
                before: None,
                after: None,
                submitted: Some(true),
                start_cursor: 0,
                parent_target: None,
                parent_session_ref,
                stall_after_ms: None,
                max_runtime_ms: None,
                on_budget: None,
                request_id: None,
                foreground_blocker: None,
            })
            .unwrap()
            .expect("store present, so the task is registered");
        task.lifecycle = lifecycle.clone();
        task.terminal_status = Some(lifecycle.as_str().to_owned());
        task.terminal_at_ms = Some(updated_at_ms);
        task.updated_at_ms = updated_at_ms;
        let encoded = serde_json::to_string(&task).unwrap();
        store
            .lock()
            .unwrap()
            .put_operation_ledger(OperationLedgerInput {
                op_id: task_id,
                kind: PROMPT_TASK_OPERATION_KIND,
                idempotency_key: None,
                request_hash: None,
                phase: "terminal",
                state: lifecycle.as_str(),
                result_json: &encoded,
                now_ms: i64::try_from(updated_at_ms).unwrap_or(i64::MAX),
                expires_at,
            })
            .unwrap();
    }

    fn s4_query<'a>(
        parent_session_ref: Option<&'a str>,
        receipt_id: Option<&'a str>,
        status: Option<&'a str>,
        since_ms: Option<u64>,
    ) -> InboxQuery<'a> {
        InboxQuery {
            workspace_id: None,
            parent_target: None,
            parent_session_ref,
            include_acknowledged: true,
            advisory: false,
            limit: 50,
            receipt_id,
            status,
            since_ms,
        }
    }

    /// S4 the shipped ready criterion: the agent handshake is immediate, the
    /// self-reported idle status must hold for the settle window.
    #[test]
    fn spawn_ready_criterion_needs_the_settle_window_for_idle_only_evidence() {
        assert!(spawn_readiness_satisfied(true, None, 1_000));
        assert!(!spawn_readiness_satisfied(false, None, 100_000));
        assert!(!spawn_readiness_satisfied(false, Some(1_000), 1_100));
        assert!(!spawn_readiness_satisfied(
            false,
            Some(1_000),
            1_000 + SPAWN_READY_SETTLE_MS - 1
        ));
        assert!(spawn_readiness_satisfied(
            false,
            Some(1_000),
            1_000 + SPAWN_READY_SETTLE_MS
        ));
    }

    /// S4 a delivery-uncertain row explains how to confirm it, and a settled row
    /// carries no such ambiguity.
    #[test]
    fn delivery_uncertain_rows_carry_a_confirm_hint() {
        let root = spawn_project_root("confirm-hint");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        seed_task_row(
            &registry,
            &store,
            "t-uncertain",
            "w1:p9",
            None,
            PromptTaskLifecycle::DeliveryUncertain,
            1_000,
            None,
        );
        seed_task_row(
            &registry,
            &store,
            "t-completed",
            "w1:p9",
            None,
            PromptTaskLifecycle::Completed,
            1_000,
            None,
        );
        let uncertain = registry.task_status("t-uncertain");
        let row = &uncertain["task"];
        assert_eq!(row["confirm_hint"]["action"], json!("herdr_mcp.agent.read"));
        assert_eq!(row["confirm_hint"]["target"], json!("w1:p9"));
        assert!(row["confirm_hint"].get("cursor").is_some(), "{row}");
        let completed = registry.task_status("t-completed");
        assert!(
            completed["task"].get("confirm_hint").is_none(),
            "{completed}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 inbox filters: receipt id, status and timestamp narrow one scope.
    #[test]
    fn inbox_filters_select_by_receipt_status_and_since() {
        let root = spawn_project_root("inbox-filters");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        for (task_id, lifecycle, updated) in [
            ("t-done", PromptTaskLifecycle::Completed, 1_000_u64),
            ("t-failed", PromptTaskLifecycle::Failed, 2_000),
            ("t-blocked", PromptTaskLifecycle::Blocked, 3_000),
        ] {
            seed_task_row(
                &registry,
                &store,
                task_id,
                "w1:p9",
                Some("session-a"),
                lifecycle,
                updated,
                None,
            );
        }

        let by_receipt = registry.task_inbox_with_query(&s4_query(
            Some("session-a"),
            Some("t-failed"),
            None,
            None,
        ));
        assert_eq!(by_receipt["ok"], true, "{by_receipt}");
        assert_eq!(by_receipt["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(by_receipt["tasks"][0]["receipt_id"], json!("t-failed"));

        let by_status = registry.task_inbox_with_query(&s4_query(
            Some("session-a"),
            None,
            Some("blocked"),
            None,
        ));
        assert_eq!(
            by_status["tasks"].as_array().unwrap().len(),
            1,
            "{by_status}"
        );
        assert_eq!(by_status["tasks"][0]["receipt_id"], json!("t-blocked"));

        let by_since =
            registry.task_inbox_with_query(&s4_query(Some("session-a"), None, None, Some(1_500)));
        let mut ids = by_since["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|task| task["receipt_id"].as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec!["t-blocked", "t-failed"], "{by_since}");

        // The bounded wait honors the same receipt filter: only this receipt
        // can wake it, even though the scope has other pending terminals.
        let woke = registry.task_inbox_waiting(
            &s4_query(Some("session-a"), Some("t-blocked"), None, None),
            10,
        );
        assert_eq!(woke["wait"]["woke"], json!("terminal_task"), "{woke}");
        assert_eq!(woke["tasks"].as_array().unwrap().len(), 1, "{woke}");
        assert_eq!(woke["tasks"][0]["receipt_id"], json!("t-blocked"));
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 archive: a terminal receipt past retention leaves the default query,
    /// stays out of the delete path, and remains queryable through the archive.
    #[test]
    fn archived_terminal_receipts_leave_default_queries_but_stay_queryable() {
        let root = spawn_project_root("inbox-archive");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        seed_task_row(
            &registry,
            &store,
            "t-live",
            "w1:p9",
            Some("session-a"),
            PromptTaskLifecycle::Completed,
            5_000,
            None,
        );
        // Retention deadline already elapsed.
        seed_task_row(
            &registry,
            &store,
            "t-old",
            "w1:p9",
            Some("session-a"),
            PromptTaskLifecycle::Completed,
            1_000,
            Some(1),
        );

        let default =
            registry.task_inbox_with_query(&s4_query(Some("session-a"), None, None, None));
        let ids = default["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|task| task["receipt_id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["t-live"], "{default}");

        let archive = registry.archive_payload(&s4_query(Some("session-a"), None, None, None));
        assert_eq!(archive["ok"], true, "{archive}");
        assert_eq!(archive["archive"], json!(true), "{archive}");
        let archived = archive["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|task| task["receipt_id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(archived, vec!["t-old"], "{archive}");
        assert_eq!(
            archive["tasks"][0]["receipt_id"],
            json!("t-old"),
            "the archived row keeps its receipt id"
        );
        // S4 review P2-4: the embedded stats describe the live scope too, not
        // only the archived count.
        assert_eq!(archive["stats"]["completed"], json!(1), "{archive}");
        assert_eq!(archive["stats"]["terminal"], json!(1), "{archive}");
        assert_eq!(archive["stats"]["archived"], json!(1), "{archive}");

        // Retention never deletes the row: it is still physically present.
        let still_stored = store
            .lock()
            .unwrap()
            .operation_ledger("t-old")
            .unwrap()
            .expect("archived rows are retained, not deleted");
        assert_eq!(still_stored.kind, PROMPT_TASK_OPERATION_KIND);

        // Archive filters compose with the same receipt/status filters.
        let by_receipt =
            registry.archive_payload(&s4_query(Some("session-a"), Some("t-live"), None, None));
        assert_eq!(
            by_receipt["tasks"].as_array().unwrap().len(),
            0,
            "{by_receipt}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 stats: read-only counters, no task bodies.
    #[test]
    fn inbox_stats_are_read_only_counters() {
        let root = spawn_project_root("inbox-stats");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        for (task_id, lifecycle, expires) in [
            ("t-a", PromptTaskLifecycle::Completed, None),
            ("t-b", PromptTaskLifecycle::Failed, None),
            ("t-c", PromptTaskLifecycle::DeliveryUncertain, None),
            ("t-old", PromptTaskLifecycle::Completed, Some(1)),
        ] {
            seed_task_row(
                &registry,
                &store,
                task_id,
                "w1:p9",
                Some("session-a"),
                lifecycle,
                1_000,
                expires,
            );
        }
        let stats = registry.stats_payload(&s4_query(Some("session-a"), None, None, None));
        assert_eq!(stats["ok"], true, "{stats}");
        assert!(
            stats.get("tasks").is_none(),
            "stats returns no task rows: {stats}"
        );
        let counters = &stats["stats"];
        assert_eq!(counters["read_only"], json!(true));
        // Counters describe the rows the default queries still serve; the
        // archived receipt is counted separately and never inflates them.
        assert_eq!(counters["completed"], json!(1), "{stats}");
        assert_eq!(counters["failed"], json!(1), "{stats}");
        assert_eq!(counters["delivery_uncertain"], json!(1), "{stats}");
        assert_eq!(counters["archived"], json!(1), "{stats}");
        assert_eq!(counters["terminal"], json!(3), "{stats}");
        assert_eq!(counters["retention_ms"], json!(prompt_task_ttl_ms()));
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 review P1-1: a repair dispatch that fails parameter validation must
    /// not spend a repair attempt, so invalid input cannot burn a receipt lane.
    #[test]
    fn invalid_repair_dispatch_does_not_spend_the_budget() {
        let root = spawn_project_root("repair-invalid");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        seed_task_row(
            &registry,
            &store,
            "t-repair",
            "w1:p9",
            None,
            PromptTaskLifecycle::Failed,
            1_000,
            None,
        );
        let client = HerdrClient::new(temp_socket());

        // Missing text, invalid stall_after_ms and an invalid wait each reject
        // the request before admission.
        for (label, args) in [
            (
                "missing text",
                json!({"target": "w1:p9", "repair_of": "t-repair"}),
            ),
            (
                "invalid stall_after_ms",
                json!({
                    "target": "w1:p9",
                    "text": "repair",
                    "repair_of": "t-repair",
                    "stall_after_ms": 1,
                }),
            ),
            (
                "invalid wait",
                json!({
                    "target": "w1:p9",
                    "text": "repair",
                    "repair_of": "t-repair",
                    "wait": {"timeout_ms": 0},
                }),
            ),
        ] {
            for _ in 0..REPAIR_BUDGET_ATTEMPTS {
                let rejected = run(&client, &registry, &args);
                assert_eq!(rejected["ok"], false, "{label}: {rejected}");
                assert_eq!(rejected["code"], "invalid_params", "{label}: {rejected}");
            }
        }

        let receipt = registry.task_status("t-repair")["task"].clone();
        assert!(
            receipt.get("repair").is_none(),
            "invalid requests must not spend a repair attempt: {receipt}"
        );
        // The lane is still fully available.
        for attempt in 1..=REPAIR_BUDGET_ATTEMPTS {
            let spent = registry.record_repair_attempt("t-repair");
            assert_eq!(spent["ok"], true, "attempt {attempt}: {spent}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 review P1-2: concurrent repairs must never exceed the per-receipt
    /// budget. The count/check/increment runs in one store transaction.
    #[test]
    fn concurrent_repairs_cannot_exceed_the_receipt_budget() {
        let root = spawn_project_root("repair-concurrent");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let registry = Arc::new(registry);
        let store = registry.inner.store.clone().unwrap();
        seed_task_row(
            &registry,
            &store,
            "t-repair",
            "w1:p9",
            None,
            PromptTaskLifecycle::Failed,
            1_000,
            None,
        );

        let mut handles = Vec::new();
        for _ in 0..12 {
            let registry = Arc::clone(&registry);
            handles.push(std::thread::spawn(move || {
                registry.record_repair_attempt("t-repair")
            }));
        }
        let mut accepted = 0_u32;
        let mut exhausted = 0_u32;
        for handle in handles {
            let outcome = handle.join().expect("repair thread");
            if outcome["ok"] == json!(true) {
                accepted += 1;
            } else {
                assert_eq!(
                    outcome["code"],
                    json!("repair_budget_exhausted"),
                    "{outcome}"
                );
                exhausted += 1;
            }
        }
        assert_eq!(accepted, REPAIR_BUDGET_ATTEMPTS, "budget overspent");
        assert_eq!(accepted + exhausted, 12);
        let receipt = registry.task_status("t-repair")["task"].clone();
        assert_eq!(receipt["repair"]["attempts"], json!(REPAIR_BUDGET_ATTEMPTS));
        assert_eq!(receipt["repair"]["exhausted"], json!(true));
        fs::remove_dir_all(root).unwrap();
    }

    /// S4 repair budget: three repairs per receipt, then the lane is terminal and
    /// the next repair is refused before any delivery attempt.
    #[test]
    fn repair_budget_is_per_receipt_and_terminal_on_exhaustion() {
        let root = spawn_project_root("repair-budget");
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store, &root, 80);
        let store = registry.inner.store.clone().unwrap();
        seed_task_row(
            &registry,
            &store,
            "t-repair",
            "w1:p9",
            None,
            PromptTaskLifecycle::Failed,
            1_000,
            None,
        );

        for attempt in 1..=REPAIR_BUDGET_ATTEMPTS {
            let spent = registry.record_repair_attempt("t-repair");
            assert_eq!(spent["ok"], true, "attempt {attempt}: {spent}");
            assert_eq!(spent["attempts"], json!(attempt), "{spent}");
        }
        let exhausted = registry.record_repair_attempt("t-repair");
        assert_eq!(exhausted["ok"], false, "{exhausted}");
        assert_eq!(
            exhausted["code"],
            json!("repair_budget_exhausted"),
            "{exhausted}"
        );
        assert_eq!(exhausted["terminal"], json!(true), "{exhausted}");
        assert_eq!(
            exhausted["delivery_state"],
            json!("not_delivered"),
            "an exhausted budget never attempts delivery: {exhausted}"
        );
        assert_eq!(exhausted["retryable"], json!(false), "{exhausted}");

        let receipt = registry.task_status("t-repair")["task"].clone();
        assert_eq!(receipt["repair"]["attempts"], json!(REPAIR_BUDGET_ATTEMPTS));
        assert_eq!(receipt["repair"]["exhausted"], json!(true));

        // A running receipt cannot be repaired at all.
        seed_task_row(
            &registry,
            &store,
            "t-running",
            "w1:p9",
            None,
            PromptTaskLifecycle::Running,
            2_000,
            None,
        );
        let rejected = registry.record_repair_attempt("t-running");
        assert_eq!(
            rejected["code"],
            json!("repair_target_not_terminal"),
            "{rejected}"
        );
        assert_eq!(rejected["delivery_state"], json!("not_delivered"));
        fs::remove_dir_all(root).unwrap();
    }

    fn spawn_agent_started(ready: bool) -> Value {
        json!({"result": {
            "type": "agent_started",
            "argv": [],
            "agent": {
                "pane_id": "w1:p9",
                "tab_id": "w1:t2",
                "workspace_id": "w1",
                "agent_status": if ready { "idle" } else { "working" },
                "interactive_ready": ready,
                "state_change_seq": 1,
            },
        }})
    }

    fn spawn_prompt_submitted() -> Value {
        json!({"result": {"prompt": {"status": "submitted", "agent": {
            "pane_id": "w1:p9", "agent_status": "working", "state_change_seq": 2,
        }}}})
    }

    fn spawn_step_names(facts: &Value) -> Vec<String> {
        facts["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["step"].as_str().map(str::to_owned))
            .collect()
    }

    fn refused_spawn_server(socket: &Path) -> UnixListener {
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }

    /// user planner | Given one spawn dispatch | When the runtime creates the
    /// pane, starts the agent and prompts it | Then one call replaces
    /// inspect/split/start/dispatch and records exactly one durable task.
    #[test]
    fn spawn_dispatch_creates_one_pane_agent_and_task() {
        let root = spawn_project_root("happy");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                spawn_tab_created(),
                spawn_agent_started(true),
                spawn_prompt_submitted(),
            ],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let args = json!({
            "spawn": {
                "kind": "pi",
                "workspace_id": "demo-project",
                "cwd": root.to_string_lossy(),
                "tab_label": "feat-x",
            },
            "text": "implement the task",
            "idempotency_key": "spawn-happy",
        });
        let result = run(&HerdrClient::new(&socket), &registry, &args);
        server.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["target"], "w1:p9");
        assert_eq!(result["spawn"]["pane_id"], "w1:p9");
        assert_eq!(result["spawn"]["tab_id"], "w1:t2");
        assert_eq!(result["spawn"]["kind"], "pi");
        assert_eq!(result["spawn"]["workspace_id"], "w1");
        assert_eq!(result["spawn"]["created"], true);
        assert_eq!(
            spawn_step_names(&result["spawn"]),
            vec!["pane_create", "agent_start", "agent_ready", "prompt"]
        );
        // S4: one call, one receipt - the spawn chain and its delivery share the
        // single durable receipt id.
        assert_eq!(result["receipt_id"], result["task_id"], "{result}");
        assert!(
            result["receipt_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "{result}"
        );
        let recorded = requests.lock().unwrap();
        let methods = recorded
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["tab.create", "agent.start", "agent.prompt"]);
        assert_eq!(recorded[0]["params"]["label"], "feat-x");
        assert_eq!(recorded[0]["params"]["cwd"], json!(root.to_string_lossy()));
        assert_eq!(recorded[0]["params"]["focus"], false);
        assert_eq!(recorded[1]["params"]["kind"], "pi");
        assert_eq!(recorded[1]["params"]["pane_id"], "w1:p9");
        assert_eq!(recorded[2]["params"]["target"], "w1:p9");
        assert_eq!(recorded[2]["params"]["text"], "implement the task");
        let tasks = registry.list_tasks(10).unwrap();
        assert_eq!(tasks.len(), 1, "one spawn dispatch owns exactly one task");
        assert_eq!(tasks[0].target, "w1:p9");
        assert_eq!(tasks[0].parent_session_ref, None);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a spawn dispatch already completed | When the same
    /// idempotency key is replayed | Then the stored spawn facts return and no
    /// second tab, pane, agent or prompt is created.
    #[test]
    fn spawn_dispatch_replay_never_creates_second_pane() {
        let root = spawn_project_root("replay");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                spawn_tab_created(),
                spawn_agent_started(true),
                spawn_prompt_submitted(),
            ],
            requests.clone(),
        );
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, watch_off) = spawn_registry_with(store.clone(), &root, 80);
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1"},
            "text": "implement the task",
            "idempotency_key": "spawn-replay",
        });
        let client = HerdrClient::new(&socket);
        let first = run(&client, &registry, &args);
        server.join().unwrap();
        assert_eq!(first["ok"], true, "{first}");
        // The watch-off guard owns the crate env lock for its lifetime: it must
        // be dropped before a second registry takes the lock again.
        drop(watch_off);
        let (reloaded, _watch_off2) = spawn_registry_with(store, &root, 80);
        let replay = run(&client, &reloaded, &args);
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["spawn"]["pane_id"], "w1:p9");
        assert_eq!(replay["spawn"]["agent_name"], first["spawn"]["agent_name"]);
        assert_eq!(
            requests.lock().unwrap().len(),
            3,
            "a replay must not touch the socket again"
        );
        assert_eq!(reloaded.list_tasks(10).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given an explicit split parent | When the spawn runs |
    /// Then the pane comes from pane.split in that parent, not a new tab.
    #[test]
    fn spawn_dispatch_split_from_uses_the_named_parent_pane() {
        let root = spawn_project_root("split");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                json!({"result": {"pane": {"pane_id": "w1:p9", "tab_id": "w1:t1", "workspace_id": "w1"}}}),
                spawn_agent_started(true),
                spawn_prompt_submitted(),
            ],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 120);
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {
                    "kind": "codex",
                    "workspace_id": "w1",
                    "split_from": "w1:p1",
                    "direction": "down",
                },
                "text": "work",
                "idempotency_key": "spawn-split",
            }),
        );
        server.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["spawn"]["pane_id"], "w1:p9");
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded[0]["method"], "pane.split");
        assert_eq!(recorded[0]["params"]["target_pane_id"], "w1:p1");
        assert_eq!(recorded[0]["params"]["direction"], "down");
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a tab too narrow for a readable horizontal split |
    /// When spawning with split_from | Then the pane is refused before any
    /// native call and the keyed failure replays without creating anything.
    #[test]
    fn spawn_dispatch_narrow_split_fails_before_any_mutation() {
        let root = spawn_project_root("narrow");
        let socket = temp_socket();
        let listener = refused_spawn_server(&socket);
        let (registry, _watch_off) = spawn_registry(&root, 20);
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1", "split_from": "w1:p1"},
            "text": "work",
            "idempotency_key": "spawn-narrow",
        });
        let client = HerdrClient::new(&socket);
        let first = run(&client, &registry, &args);
        assert_eq!(first["ok"], false, "{first}");
        assert_eq!(first["failed_step"], "pane_create");
        assert_eq!(first["code"], "pane_split_too_narrow");
        assert_eq!(first["delivery_state"], "not_delivered");
        assert_eq!(first["spawn"]["created"], false);
        assert_eq!(first["spawn"]["pane_id"], Value::Null);
        assert!(first.get("reclaim").is_none(), "{first}");
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["failed_step"], "pane_create");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(registry.list_tasks(10).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given the agent cannot start in the new pane | When the
    /// spawn fails | Then the failure names agent_start, keeps the pane for
    /// reclaim, and a replay does not create another pane.
    #[test]
    fn spawn_dispatch_agent_start_failure_reports_the_created_pane() {
        let root = spawn_project_root("startfail");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                spawn_tab_created(),
                json!({"error": {"code": "agent_start_failed", "message": "kind pi is not installed"}}),
            ],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1"},
            "text": "work",
            "idempotency_key": "spawn-startfail",
        });
        let client = HerdrClient::new(&socket);
        let first = run(&client, &registry, &args);
        server.join().unwrap();
        assert_eq!(first["ok"], false, "{first}");
        assert_eq!(first["failed_step"], "agent_start");
        assert_eq!(first["code"], "agent_start_failed");
        assert_eq!(first["delivery_state"], "not_delivered");
        assert_eq!(first["spawn"]["pane_id"], "w1:p9");
        assert_eq!(first["spawn"]["created"], true);
        assert_eq!(first["spawn"]["agent_started"], false);
        assert_eq!(first["reclaim"]["pane_id"], "w1:p9");
        assert_eq!(spawn_step_names(&first["spawn"]), vec!["pane_create"]);
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["failed_step"], "agent_start");
        assert_eq!(requests.lock().unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// user keeps the release gate green | Given a scripted fake whose client
    /// hangs up before the reply lands | When the fake writes that reply | Then
    /// it observes the closed peer and stops instead of panicking the test
    /// thread, which is the shape that failed the v1.1.0-rc.13 release gate.
    #[test]
    fn spawn_scripted_fake_survives_a_client_that_hangs_up_before_the_reply() {
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![spawn_tab_created()],
            requests.clone(),
        );
        {
            let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            writeln!(client, "{}", json!({"id": 1, "method": "tab.create"})).unwrap();
            client.shutdown(std::net::Shutdown::Both).unwrap();
        }
        server
            .join()
            .expect("a client that hung up must not panic the fake socket thread");
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "the request the client did send must still be recorded"
        );
    }

    /// user planner | Given a chain budget too small to start an agent | When
    /// the spawn returns | Then it persists a resumable checkpoint (no
    /// in-flight key, no agent.start) and the same key replays into that pane.
    #[test]
    fn spawn_dispatch_budget_pause_replays_into_the_same_pane() {
        let root = spawn_project_root("budgetpause");
        let socket = temp_socket();
        let first_requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![spawn_tab_created()],
            first_requests.clone(),
        );
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store.clone(), &root, 80);
        let client = HerdrClient::new(&socket);
        let mut args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1", "startup_timeout_ms": 1},
            "text": "work",
            "idempotency_key": "spawn-budget-pause",
        });
        let paused = run(&client, &registry, &args);
        server.join().unwrap();
        assert_eq!(paused["ok"], false, "{paused}");
        assert_eq!(paused["failed_step"], "agent_start", "{paused}");
        assert_eq!(paused["phase"], "agent_start", "{paused}");
        assert_eq!(paused["resumable"], true, "{paused}");
        assert_eq!(paused["code"], "spawn_budget_exhausted", "{paused}");
        assert_eq!(paused["delivery_state"], "not_delivered");
        assert_eq!(paused["spawn"]["pane_id"], "w1:p9");
        assert_eq!(paused["spawn"]["agent_started"], false);
        assert_eq!(paused["reclaim"]["pane_id"], "w1:p9");
        assert_eq!(
            spawn_requested_methods(&first_requests),
            vec!["tab.create"],
            "a budget that cannot start an agent must not call agent.start"
        );

        // Replaying the SAME key with a larger chain continues in that pane.
        // The resume budget must also leave the prompt reserve after the
        // bounded early-exit watch (1.2 S5), so it is larger than the old
        // start-only floor.
        args["spawn"]["startup_timeout_ms"] = json!(20_000);
        fs::remove_file(&socket).unwrap();
        let resume_requests = Arc::new(Mutex::new(Vec::new()));
        let resume_server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                json!({"error": {"code": "agent_not_found", "message": "no agent yet"}}),
                spawn_agent_started(true),
                spawn_prompt_submitted(),
            ],
            resume_requests.clone(),
        );
        let resumed = run(&client, &registry, &args);
        resume_server.join().unwrap();
        assert_eq!(resumed["ok"], true, "{resumed}");
        assert_eq!(resumed["spawn"]["pane_id"], "w1:p9");
        assert_eq!(resumed["spawn"]["resumed"], true, "{resumed}");
        assert_eq!(
            spawn_requested_methods(&resume_requests),
            vec!["agent.get", "agent.start", "agent.prompt"],
            "a resume reuses the persisted pane and never creates another"
        );
        let resume_step = resumed["spawn"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["step"] == "resume")
            .unwrap_or_else(|| panic!("resume step missing: {resumed}"));
        assert_eq!(resume_step["phase"], "agent_start");
        let tasks = registry.list_tasks(10).unwrap();
        assert_eq!(tasks.len(), 1, "one spawn dispatch owns one durable task");
        assert_eq!(tasks[0].target, "w1:p9");
        // A later replay is now a pure replay: nothing more is submitted.
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(spawn_requested_methods(&resume_requests).len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a chain that runs out of the caller's budget while
    /// the agent is still starting | When the spawn returns | Then it reports a
    /// resumable agent_ready checkpoint and the same key later prompts once.
    #[test]
    fn spawn_dispatch_readiness_pause_resumes_and_prompts_once() {
        let root = spawn_project_root("readypause");
        let socket = temp_socket();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_requests = Arc::new(Mutex::new(Vec::new()));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("tab.create", spawn_tab_created()["result"].clone()),
                ("agent.start", spawn_agent_started(false)["result"].clone()),
                (
                    "agent.get",
                    json!({"agent": {
                        "pane_id": "w1:p9", "agent_status": "working",
                        "interactive_ready": false, "state_change_seq": 1,
                    }}),
                ),
            ],
            first_requests.clone(),
        );
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, _watch_off) = spawn_registry_with(store.clone(), &root, 80);
        let client = HerdrClient::new(&socket);
        // 4.2s: enough to create the pane and start the agent, not enough to
        // finish readiness, so the chain must pause instead of holding the key.
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1", "startup_timeout_ms": 4_200},
            "text": "work",
            "idempotency_key": "spawn-ready-pause",
        });
        let paused = run(&client, &registry, &args);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        assert_eq!(paused["ok"], false, "{paused}");
        assert_eq!(paused["failed_step"], "agent_ready", "{paused}");
        assert_eq!(paused["resumable"], true, "{paused}");
        // S4: a readiness timeout names the cause and is definitely not
        // delivered - never the uncertain-delivery bucket.
        assert_eq!(paused["code"], "agent_not_ready", "{paused}");
        assert_eq!(paused["delivery_state"], "not_delivered", "{paused}");
        assert_eq!(paused["submitted"], false, "{paused}");
        assert!(
            paused.get("delivery_uncertain").is_none(),
            "a readiness timeout must never be uncertain: {paused}"
        );
        assert_eq!(
            paused["receipt_id"], paused["task_id"],
            "the failed spawn still returns its single receipt: {paused}"
        );
        assert_eq!(paused["spawn"]["pane_id"], "w1:p9");
        assert_eq!(paused["spawn"]["agent_started"], true);
        assert_eq!(paused["reclaim"]["pane_id"], "w1:p9");
        let first_methods = spawn_requested_methods(&first_requests);
        assert_eq!(first_methods[0], "tab.create");
        assert_eq!(first_methods[1], "agent.start");
        assert!(
            first_methods
                .iter()
                .filter(|method| *method == "agent.get")
                .count()
                >= 1,
            "the readiness loop must have polled: {first_methods:?}"
        );
        assert!(!first_methods.iter().any(|method| method == "agent.prompt"));

        // The replay re-checks readiness and prompts the SAME pane exactly once.
        fs::remove_file(&socket).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resume_requests = Arc::new(Mutex::new(Vec::new()));
        let resume_server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                (
                    "agent.get",
                    json!({"agent": {
                        "pane_id": "w1:p9", "agent_status": "idle",
                        "interactive_ready": true, "state_change_seq": 2,
                    }}),
                ),
                ("agent.prompt", spawn_prompt_submitted()["result"].clone()),
            ],
            resume_requests.clone(),
        );
        let resumed = run(&client, &registry, &args);
        stop.store(true, Ordering::SeqCst);
        resume_server.join().unwrap();
        assert_eq!(resumed["ok"], true, "{resumed}");
        assert_eq!(resumed["spawn"]["pane_id"], "w1:p9");
        assert_eq!(resumed["spawn"]["resumed"], true, "{resumed}");
        assert_eq!(
            spawn_requested_methods(&resume_requests),
            vec!["agent.get", "agent.prompt"],
            "a resume must not create a pane or restart the agent"
        );
        let tasks = registry.list_tasks(10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].target, "w1:p9");
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(spawn_requested_methods(&resume_requests).len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given the spawn chain budget plan | When the steps divide
    /// the caller's window | Then every step stays inside it, the default fits
    /// the smallest WebChat transport, and the ceiling stays under the CLI cap.
    #[test]
    fn spawn_chain_budget_fits_the_caller_transport() {
        // WebChat herdr_call without timeout_ms is cut by Edge at 30s.
        let default_fits_webchat = SPAWN_CHAIN_DEFAULT_MS < 30_000;
        assert!(default_fits_webchat);
        // The local CLI waits chain + 10s slack and is capped at 120s.
        let ceiling_fits_cli_cap = SPAWN_CHAIN_MAX_MS + 10_000 <= 120_000;
        assert!(ceiling_fits_cli_cap);
        assert_eq!(spawn_prompt_reserve_ms(SPAWN_CHAIN_DEFAULT_MS), 5_000);
        assert_eq!(
            spawn_step_budget_ms(SPAWN_CHAIN_DEFAULT_MS, SPAWN_CHAIN_DEFAULT_MS),
            SPAWN_CHAIN_DEFAULT_MS - 5_000
        );
        // No step may ever exceed what the caller still waits for.
        for chain in [1_u64, 100, 4_001, 25_000, SPAWN_CHAIN_MAX_MS] {
            for remaining in [1_u64, 250, 4_001, chain] {
                let step = spawn_step_budget_ms(chain, remaining);
                assert!(step <= remaining, "chain {chain} remaining {remaining}");
                assert!(
                    spawn_prompt_reserve_ms(chain) <= chain / 4 + 1,
                    "the prompt reserve stays a quarter of the chain"
                );
            }
        }
        // A tiny chain cannot host a native agent.start: the chain reports a
        // resumable checkpoint instead of reserving work the caller drops.
        assert!(
            spawn_step_budget_ms(1, 1) < SPAWN_NATIVE_START_FLOOR_MS,
            "a 1ms chain must pause before agent.start"
        );
    }

    /// user worker | Given a two-level delegation chain | When a spawn dispatch
    /// would add a third level | Then the ledger guard refuses it before any
    /// pane exists, with no phantom prompt step and nothing to reclaim.
    #[test]
    fn spawn_dispatch_delegation_refusal_happens_before_any_pane() {
        let root = spawn_project_root("delegation");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![json!({"agent": {"pane_id": "w1:p2", "agent_status": "working"}})],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 80);
        register_running_task(
            &registry,
            "supervisor-task",
            "supervisor",
            "w1:p1",
            Some("planner"),
            None,
            0,
        );
        register_running_task(
            &registry,
            "worker-task",
            "worker",
            "w1:p2",
            Some("supervisor"),
            None,
            0,
        );
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1"},
                "text": "work",
                "idempotency_key": "spawn-delegation",
                "parent_target": "worker",
            }),
        );
        server.join().unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "delegation_depth_exceeded", "{result}");
        assert!(
            result.get("failed_step").is_none(),
            "a pre-pane refusal is not a spawn step failure: {result}"
        );
        assert!(result.get("spawn").is_none(), "{result}");
        assert_eq!(
            spawn_requested_methods(&requests),
            vec!["agent.get"],
            "only the caller probe runs; no pane, agent or prompt is created"
        );
        assert_eq!(registry.list_tasks(10).unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a spawn dispatch refused by the delegation guard
    /// before any pane existed | When the same idempotency key is replayed
    /// inside the TTL | Then the stored refusal returns instead of an in-flight
    /// reservation, and nothing is probed, created or prompted again.
    #[test]
    fn spawn_dispatch_delegation_refusal_replays_under_the_same_key() {
        let root = spawn_project_root("delegation-replay");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![json!({"agent": {"pane_id": "w1:p2", "agent_status": "working"}})],
            requests.clone(),
        );
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (registry, watch_off) = spawn_registry_with(store.clone(), &root, 80);
        register_running_task(
            &registry,
            "supervisor-task",
            "supervisor",
            "w1:p1",
            Some("planner"),
            None,
            0,
        );
        register_running_task(
            &registry,
            "worker-task",
            "worker",
            "w1:p2",
            Some("supervisor"),
            None,
            0,
        );
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1"},
            "text": "work",
            "idempotency_key": "spawn-delegation-replay",
            "parent_target": "worker",
        });
        let client = HerdrClient::new(&socket);
        let first = run(&client, &registry, &args);
        server.join().unwrap();
        assert_eq!(first["ok"], false, "{first}");
        assert_eq!(first["code"], "delegation_depth_exceeded", "{first}");
        assert!(
            first.get("idempotent_replay").is_none(),
            "the first refusal is an outcome, not a replay: {first}"
        );

        // A fresh registry over the same durable store proves the refusal was
        // persisted for the key rather than remembered inside one process.
        // The watch-off guard owns the crate env lock for its lifetime: drop it
        // before the second registry takes the lock again.
        drop(watch_off);
        let (reloaded, _watch_off2) = spawn_registry_with(store, &root, 80);
        let replay = run(&client, &reloaded, &args);
        assert_eq!(replay["ok"], false, "{replay}");
        assert_eq!(replay["code"], "delegation_depth_exceeded", "{replay}");
        assert_eq!(replay["message"], first["message"], "{replay}");
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["dispatch_id"], first["dispatch_id"], "{replay}");
        assert_eq!(
            spawn_requested_methods(&requests),
            vec!["agent.get"],
            "the replay is a pure observation: no second caller probe, pane, agent or prompt"
        );
        assert_eq!(reloaded.list_tasks(10).unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given the prompt step fails after the pane exists | When
    /// the spawn dispatch returns | Then it reports failed_step prompt and the
    /// created pane, and never claims delivery.
    #[test]
    fn spawn_dispatch_prompt_failure_reports_the_prompt_step() {
        let root = spawn_project_root("promptfail");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            vec![
                spawn_tab_created(),
                spawn_agent_started(true),
                json!({"error": {"code": "agent_not_ready", "message": "agent refused input"}}),
            ],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1"},
                "text": "work",
                "idempotency_key": "spawn-promptfail",
            }),
        );
        server.join().unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["failed_step"], "prompt");
        assert_eq!(result["code"], "agent_not_ready");
        // An agent.prompt transport failure is uncertain, never not_delivered:
        // the spawn facts still name the pane so the caller can verify it.
        assert_eq!(result["delivery_uncertain"], true);
        assert!(result.get("delivery_state").is_none(), "{result}");
        assert!(result.get("submitted").is_none(), "{result}");
        assert_eq!(result["spawn"]["pane_id"], "w1:p9");
        assert_eq!(
            spawn_step_names(&result["spawn"]),
            vec!["pane_create", "agent_start", "agent_ready", "prompt"]
        );
        assert_eq!(result["spawn"]["steps"][3]["ok"], false);
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given spawn combined with target, a missing idempotency
    /// key, or a cwd outside the managed roots | When dispatching | Then the
    /// validate step fails with no reservation and no native call.
    #[test]
    fn spawn_dispatch_validation_failures_have_no_side_effects() {
        let root = spawn_project_root("validate");
        let socket = temp_socket();
        let listener = refused_spawn_server(&socket);
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let client = HerdrClient::new(&socket);
        for (args, expected) in [
            (
                json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1"},
                    "target": "w1:p1",
                    "text": "work",
                    "idempotency_key": "spawn-conflict",
                }),
                "mutually exclusive",
            ),
            (
                json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1"},
                    "text": "work",
                }),
                "idempotency_key is required with spawn",
            ),
            (
                json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1", "cwd": "/etc"},
                    "text": "work",
                    "idempotency_key": "spawn-outside",
                }),
                "managed project root",
            ),
            (
                json!({
                    "spawn": {"kind": "PI", "workspace_id": "w1"},
                    "text": "work",
                    "idempotency_key": "spawn-kind",
                }),
                "spawn.kind",
            ),
            (
                json!({
                    "spawn": {"workspace_id": "w1"},
                    "text": "work",
                    "idempotency_key": "spawn-nokind",
                }),
                "spawn.kind is required",
            ),
            (
                json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1"},
                    "text": "work",
                    "idempotency_key": "spawn-cap",
                    "max_concurrent": 2,
                }),
                "max_concurrent is not applicable with spawn",
            ),
        ] {
            let result = run(&client, &registry, &args);
            assert_eq!(result["ok"], false, "{result}");
            assert_eq!(result["failed_step"], "validate", "{result}");
            assert_eq!(result["code"], "invalid_params", "{result}");
            assert!(
                result["message"].as_str().unwrap().contains(expected),
                "{result}"
            );
            assert!(result.get("spawn").is_none(), "{result}");
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(registry.list_tasks(10).unwrap().is_empty());
        // A keyed validate rejection is persisted, so the same key replays the
        // rejection instead of resolving a spawn after the topology changed.
        let replay = run(
            &client,
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1", "cwd": "/etc"},
                "text": "work",
                "idempotency_key": "spawn-outside",
            }),
        );
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["failed_step"], "validate", "{replay}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given busy and idle codex panes | When selecting a role
    /// and replaying without a snapshot | Then one concrete pane is delivered,
    /// durably recorded and retained in every task projection.
    #[test]
    fn role_dispatch_records_idle_target_and_replays_without_resolution() {
        for selector in ["@codex", "@codex:idle"] {
            let socket = temp_socket();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let server = scripted_read_server(
                UnixListener::bind(&socket).unwrap(),
                vec![role_submitted("w1:p3")],
                requests.clone(),
            );
            let client = HerdrClient::new(&socket);
            let registry = role_registry(json!([
                {"pane_id": "w1:p1", "kind": "codex", "agent_status": "working"},
                {"pane_id": "w1:p2", "kind": "pi", "agent_status": "idle"},
                {"pane_id": "w1:p3", "kind": "codex", "agent_status": "idle", "api_key": "secret"},
                {"pane_id": "w1:p4", "kind": "codex", "agent_status": "idle"},
            ]));
            let args = json!({"target": selector, "text": "work", "idempotency_key": "role-key", "max_concurrent": 1});
            let first = run(&client, &registry, &args);
            server.join().unwrap();
            assert_eq!(first["ok"], true, "{first}");
            assert_eq!(first["resolved_target"], "w1:p3");
            assert_eq!(first["task"]["target"], "w1:p3");
            assert_eq!(first["task"]["requested_target"], selector);
            assert_eq!(first["candidates"].as_array().unwrap().len(), 3);
            assert!(!first["candidates"].to_string().contains("secret"));
            assert_eq!(
                *requests.lock().unwrap(),
                vec![(
                    "agent.prompt".to_owned(),
                    json!({"target": "w1:p3", "text": "work"})
                )]
            );
            let store = registry.inner.store.as_ref().unwrap().clone();
            let reloaded = PromptRegistry::with_store(store);
            let replay = run(&client, &reloaded, &args);
            assert_eq!(replay["idempotent_replay"], true, "{replay}");
            assert_eq!(replay["resolved_target"], first["resolved_target"]);
            assert_eq!(replay["candidates"], first["candidates"]);
            let task_id = first["task_id"].as_str().unwrap();
            assert_eq!(
                reloaded.task_status(task_id)["task"]["requested_target"],
                selector
            );
            assert_eq!(
                reloaded.task_status_by_idempotency("role-key", selector, None)["task"]["target"],
                "w1:p3"
            );
            assert_eq!(
                reloaded.task_status_by_idempotency("role-key", selector, Some("other-session"))["code"],
                "agent_task_not_found"
            );
            let mut task = reloaded.load_task(task_id).unwrap().unwrap();
            assert_eq!(
                running_task_row(task.clone(), now_ms())["requested_target"],
                selector
            );
            task.lifecycle = PromptTaskLifecycle::Completed;
            reloaded.save_task(&task, None).unwrap();
            for view in [InboxView::Full, InboxView::Compact] {
                let inbox = apply_inbox_view(
                    reloaded.task_inbox(None, None, None, false, false, 10),
                    view,
                );
                assert_eq!(inbox["tasks"][0]["target"], "w1:p3");
                assert_eq!(inbox["tasks"][0]["requested_target"], selector);
            }
            fs::remove_file(socket).unwrap();
        }
    }

    /// user planner | Given own, working, blocked, unknown and other-workspace
    /// panes | When requesting @idle | Then only the scoped idle worker receives it.
    #[test]
    fn role_idle_excludes_caller_busy_unknown_and_other_workspace() {
        let socket = temp_socket();
        let texts = Arc::new(Mutex::new(Vec::new()));
        let parent = json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle"}}});
        let server = scripted_dispatch_server(
            UnixListener::bind(&socket).unwrap(),
            vec![parent, role_submitted("w1:p5")],
            texts.clone(),
        );
        let registry = role_registry(json!([
            {"pane_id": "w1:p1", "name": "caller", "kind": "codex", "agent_status": "idle", "workspace_id": "w1"},
            {"pane_id": "w1:p2", "kind": "codex", "agent_status": "working", "workspace_id": "w1"},
            {"pane_id": "w1:p3", "kind": "grok", "agent_status": "blocked", "workspace_id": "w1"},
            {"pane_id": "w1:p4", "kind": "pi", "workspace_id": "w1"},
            {"pane_id": "w2:p1", "kind": "pi", "agent_status": "idle", "workspace_id": "w2"},
            {"pane_id": "w1:p5", "kind": "droid", "agent_status": "idle", "workspace_id": "w1"},
        ]));
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "target": "@idle", "text": "work", "parent_target": "caller", "workspace_id": "w1",
            }),
        );
        server.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["resolved_target"], "w1:p5");
        assert_eq!(result["candidates"][0]["reason"], "parent_target_is_target");
        assert_eq!(*texts.lock().unwrap(), vec!["work"]);
        fs::remove_file(socket).unwrap();
    }

    /// user planner | Given no eligible pane or an unsupported selector | When
    /// dispatching | Then the socket receives no prompt and the failure is explicit.
    #[test]
    fn role_unavailable_broadcast_and_invalid_limits_never_submit() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let registry = role_registry(json!([
            {"pane_id": "w1:p1", "kind": "codex", "agent_status": "working"},
            {"pane_id": "w1:p2", "kind": "codex", "agent_status": "blocked"},
        ]));
        let client = HerdrClient::new(&socket);
        for selector in ["@codex", "@idle", "@grok", "@codex:idle"] {
            let result = run(
                &client,
                &registry,
                &json!({"target": selector, "text": "work"}),
            );
            assert_eq!(result["code"], "role_target_unavailable", "{result}");
            assert_eq!(result["delivery_state"], "not_delivered");
            assert_eq!(result["submitted"], false);
        }
        for selector in ["@all", "@all:idle", "@worktree:w1", "@codex,grok", "@"] {
            assert_eq!(
                run(
                    &client,
                    &registry,
                    &json!({"target": selector, "text": "work"})
                )["code"],
                "role_target_unsupported"
            );
        }
        for limit in [json!(0), json!(17), json!(1.5), json!("2"), Value::Null] {
            assert_eq!(
                run(
                    &client,
                    &registry,
                    &json!({"target": "@codex", "text": "work", "max_concurrent": limit})
                )["code"],
                "invalid_params"
            );
        }
        assert!(registry.list_tasks(100).unwrap().is_empty());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_file(socket).unwrap();
    }

    /// user planner | Given two live role tasks in each scope | When capping
    /// that selector | Then the third is rejected; terminal/other-scope work
    /// is ignored and opening a gate keeps a task counted.
    #[test]
    fn role_cap_counts_all_live_tasks_in_the_exact_parent_scope() {
        for (session, parent) in [
            (Some("session-a"), Some("parent-a")),
            (None, Some("parent-a")),
            (None, None),
        ] {
            let registry = role_registry(json!([]));
            for i in 0..2 {
                let mut task = escalation_task(
                    &format!("live-{i}"),
                    "w1:p1",
                    PromptTaskLifecycle::Running,
                    0,
                );
                task.requested_target = Some("@codex".to_owned());
                task.parent_session_ref = session.map(str::to_owned);
                task.parent_target = parent.map(str::to_owned);
                registry.save_task(&task, None).unwrap();
            }
            // Flood the bounded list window: the admission query must still
            // see the two live tasks even behind more than 1024 terminal rows.
            for i in 0..1025 {
                let mut task = escalation_task(
                    &format!("terminal-{i}"),
                    "w1:p1",
                    PromptTaskLifecycle::Completed,
                    0,
                );
                task.requested_target = Some("@codex".to_owned());
                task.parent_session_ref = session.map(str::to_owned);
                task.parent_target = parent.map(str::to_owned);
                registry.save_task(&task, None).unwrap();
            }
            let client = HerdrClient::new(temp_socket());
            let mut args = json!({"target": "@codex", "text": "work", "max_concurrent": 2});
            if let Some(parent) = parent {
                args["parent_target"] = json!(parent);
            }
            let result = run_with_parent_session(&client, &registry, &args, session, None);
            assert_eq!(result["code"], "concurrency_limit_reached", "{result}");
            assert_eq!(result["delivery_state"], "not_delivered");
            assert_eq!(result["submitted"], false);
            assert_eq!(
                run_with_parent_session(&client, &registry, &args, Some("other-session"), None)["code"],
                "role_target_unavailable"
            );
            if session.is_some() {
                args["parent_target"] = json!("another-pane-same-session");
                assert_eq!(
                    run_with_parent_session(&client, &registry, &args, session, None)["code"],
                    "concurrency_limit_reached"
                );
            } else if parent.is_some() {
                args["parent_target"] = json!("another-parent");
                assert_eq!(
                    run(&client, &registry, &args)["code"],
                    "role_target_unavailable"
                );
            }
            let gate = registry.gate_call(
                decision_gates::CREATE,
                &json!({
                    "task_id": "live-0", "question": "Choose", "options": ["continue", "stop"],
                }),
                session,
            );
            assert_eq!(gate["ok"], true, "{gate}");
            let mut task = registry.load_task("live-0").unwrap().unwrap();
            task.lifecycle = PromptTaskLifecycle::Completed;
            registry.save_task(&task, None).unwrap();
            let mut original = json!({"target": "@codex", "text": "work", "max_concurrent": 2});
            if let Some(parent) = parent {
                original["parent_target"] = json!(parent);
            }
            assert_eq!(
                run_with_parent_session(&client, &registry, &original, session, None)["code"],
                "concurrency_limit_reached"
            );
            assert_eq!(
                registry.gate_call(
                    decision_gates::CANCEL,
                    &json!({"gate_id": gate["gate"]["gate_id"]}),
                    session
                )["ok"],
                true
            );
            // No candidate now proves admission passed with one live task.
            let mut original = json!({"target": "@codex", "text": "work", "max_concurrent": 2});
            if let Some(parent) = parent {
                original["parent_target"] = json!(parent);
            }
            assert_eq!(
                run_with_parent_session(&client, &registry, &original, session, None)["code"],
                "role_target_unavailable"
            );
        }
    }

    /// user worker | Given a two-level delegation chain | When attempting a
    /// role sub-worker | Then the existing depth guard refuses every candidate.
    #[test]
    fn role_dispatch_preserves_delegation_depth_and_self_exclusion() {
        let socket = temp_socket();
        let texts = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_dispatch_server(
            UnixListener::bind(&socket).unwrap(),
            vec![json!({"result": {"agent": {"pane_id": "w1:p2", "agent_status": "idle"}}})],
            texts.clone(),
        );
        let registry = role_registry(json!([
            {"pane_id": "w1:p2", "name": "worker", "kind": "codex", "agent_status": "idle"},
            {"pane_id": "w1:p3", "kind": "pi", "agent_status": "idle"},
        ]));
        register_running_task(
            &registry,
            "supervisor-task",
            "supervisor",
            "w1:p1",
            Some("planner"),
            None,
            0,
        );
        register_running_task(
            &registry,
            "worker-task",
            "worker",
            "w1:p2",
            Some("supervisor"),
            None,
            0,
        );
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({"target": "@idle", "text": "work", "parent_target": "worker"}),
        );
        server.join().unwrap();
        assert_eq!(result["code"], "role_target_unavailable", "{result}");
        assert_eq!(result["candidates"][0]["reason"], "parent_target_is_target");
        assert_eq!(
            result["candidates"][1]["reason"],
            "delegation_depth_exceeded"
        );
        assert!(texts.lock().unwrap().is_empty());
        fs::remove_file(socket).unwrap();
    }

    /// user planner | Given simultaneous role requests and a cap of one | When
    /// both dispatch | Then only one native submission crosses the socket.
    #[test]
    fn role_cap_serializes_simultaneous_dispatches() {
        let socket = temp_socket();
        let texts = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_dispatch_server(
            UnixListener::bind(&socket).unwrap(),
            vec![role_submitted("w1:p1")],
            texts.clone(),
        );
        let registry = role_registry(json!([
            {"pane_id": "w1:p1", "kind": "codex", "agent_status": "idle"},
        ]));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers = (0..2).map(|i| {
            let registry = registry.clone();
            let client = HerdrClient::new(&socket);
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                run(&client, &registry, &json!({"target": "@codex", "text": format!("work-{i}"), "max_concurrent": 1}))
            })
        }).collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        server.join().unwrap();
        assert_eq!(
            results.iter().filter(|result| result["ok"] == true).count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result["code"] == "concurrency_limit_reached")
                .count(),
            1
        );
        assert_eq!(texts.lock().unwrap().len(), 1);
        fs::remove_file(socket).unwrap();
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
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
                if !write_fake_reply(
                    &mut stream,
                    &json!({"id": request["id"], "result": result}).to_string(),
                ) {
                    break;
                }
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
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
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
                if !write_fake_reply(
                    &mut stream,
                    &json!({"id": request["id"], "result": result}).to_string(),
                ) {
                    break;
                }
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
            kind: None,
        };
        let after = before.clone();
        let task_id = "dispatch:prompt:task-lifecycle-test";
        let initial = registry
            .register_task(RegisterTaskInput {
                task_id,
                target: "child",
                requested_target: None,
                fingerprint: "fingerprint",
                idempotency_key: Some("review-project-19-sha"),
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor: 100,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
                request_id: None,
                max_runtime_ms: None,
                on_budget: None,
                foreground_blocker: None,
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
            kind: None,
        };
        let task_id = "dispatch:prompt:activity-timeout-test";
        let mut task = registry
            .register_task(RegisterTaskInput {
                task_id,
                target: "child",
                requested_target: None,
                fingerprint: "fingerprint",
                idempotency_key: None,
                before: Some(&idle),
                after: Some(&idle),
                submitted: Some(true),
                start_cursor: 200,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
                request_id: None,
                max_runtime_ms: None,
                on_budget: None,
                foreground_blocker: None,
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
            kind: None,
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
        // The manual chain stays documented, but the one-call form is the
        // advertised recovery for a NEW agent.
        assert_eq!(
            result["recovery"]["one_shot"]["method"],
            "herdr_mcp.agent.task.dispatch"
        );
        assert_eq!(
            result["recovery"]["one_shot"]["params"]["spawn"]["kind"],
            "<agent kind>"
        );
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
            kind: None,
        };
        let after = AgentState {
            pane_id: Some("w1:p1".to_owned()),
            agent_status: Some("working".to_owned()),
            state_change_seq: Some(2),
            kind: None,
        };
        let initial = registry
            .register_task(RegisterTaskInput {
                task_id: "dispatch:prompt:boot-fence",
                target: "child",
                requested_target: None,
                fingerprint: "fp",
                idempotency_key: None,
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor: 100,
                parent_target: Some("parent"),
                parent_session_ref: None,
                stall_after_ms: None,
                request_id: None,
                max_runtime_ms: None,
                on_budget: None,
                foreground_blocker: None,
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
            requested_target: None,
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
            turn_seq: None,
            exact_native_turn: false,
            terminal_status: terminal.then(|| lifecycle.as_str().to_owned()),
            terminal_cursor: None,
            terminal_at_ms: terminal.then(|| now.saturating_sub(age_ms)),
            last_heartbeat_at_ms: None,
            worker_done_at_ms: None,
            worker_done_summary: None,
            stall_after_ms: None,
            request_id: None,
            max_runtime_ms: None,
            on_budget: None,
            budget_events: Vec::new(),
            gate: None,
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
            repair_attempts: 0,
            repair_exhausted_at_ms: None,
            steering_state: None,
            superseded_by: None,
            foreground_blocker: None,
        }
    }

    fn worker_registry(tag: &str) -> (PathBuf, PromptRegistry, Arc<AtomicU64>) {
        let (path, mut registry) = task_registry(tag);
        let clock = Arc::new(AtomicU64::new(now_ms()));
        Arc::get_mut(&mut registry.inner).unwrap().clock = Some(clock.clone());
        (path, registry, clock)
    }

    /// user parent | Given a durable task in one parent scope | When a decision
    /// gate is opened and a worker reports done | Then ack/completion stay held
    /// across reopen, other parents cannot access it, and resolution releases it.
    #[test]
    fn decision_gate_blocks_ack_completion_and_preserves_scope_across_reopen() {
        use decision_gates::{CANCEL, CREATE, RESOLVE, STATUS};
        let (path, registry, clock) = worker_registry("decision-gates");
        let mut task = escalation_task("gate-task", "child", PromptTaskLifecycle::Running, 0);
        task.parent_session_ref = Some("parent-session".to_owned());
        registry.save_task(&task, None).unwrap();
        let call = |method, params: Value, scope| {
            task_call_with_parent_session(&registry, method, &params, scope)
        };
        let create = json!({"task_id": task.task_id, "question": "Choose a path", "options": ["continue", "stop"]});
        assert_eq!(
            call(CREATE, create.clone(), Some("other-parent"))["code"],
            "agent_task_parent_mismatch"
        );
        let opened = call(CREATE, create.clone(), Some("parent-session"));
        assert_eq!(opened["ok"], true, "{opened}");
        let gate_id = opened["gate"]["gate_id"].as_str().unwrap();
        let gate_params = json!({"gate_id": gate_id});
        assert_eq!(
            call(CREATE, create, Some("parent-session"))["code"],
            "task_gate_already_open"
        );
        let status = registry.task_status(&task.task_id);
        assert_eq!(status["task"]["blocked_on_gate"], true);
        assert_eq!(status["task"]["gate"], opened["gate"]);
        assert_eq!(status["task"]["terminal"], false);
        assert_eq!(
            registry.task_ack(&task.task_id)["code"],
            "task_blocked_on_gate"
        );
        for scope in [Some("other-parent"), None] {
            assert_eq!(
                call(CANCEL, gate_params.clone(), scope)["code"],
                "agent_task_parent_mismatch"
            );
        }
        assert_eq!(
            call(STATUS, gate_params.clone(), Some("other-parent"))["code"],
            "agent_task_parent_mismatch"
        );
        assert_eq!(
            call(STATUS, gate_params.clone(), Some("parent-session"))["gate"],
            opened["gate"]
        );
        for scope in [None, Some("parent-session"), Some("other-parent")] {
            assert_eq!(
                call(
                    RESOLVE,
                    json!({"gate_id": gate_id, "resolution": "continue", "human": true}),
                    scope
                )["code"],
                "gate_resolution_requires_human"
            );
        }
        let inbox = call(
            AGENT_TASK_INBOX_METHOD,
            json!({"view": "compact"}),
            Some("parent-session"),
        );
        assert_eq!(inbox["tasks"][0]["gate"], opened["gate"]);
        assert_eq!(inbox["tasks"][0]["blocked_on_gate"], true);
        assert_eq!(inbox["summary"]["blocked"], 1);
        let running = registry.task_inbox_with_running(None, "parent-session", 50);
        assert_eq!(running["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(running["tasks"][0]["gate"], opened["gate"]);
        let report = json!({"task_id": task.task_id, "dispatch_id": task.dispatch_id, "kind": "worker_done"});
        clock.fetch_add(42, Ordering::Relaxed);
        let reported = call(AGENT_TASK_REPORT_METHOD, report.clone(), None);
        assert_eq!(reported["ok"], true);
        assert_eq!(reported["task"]["terminal"], false);
        assert_eq!(reported["task"]["lifecycle"], "running");
        assert!(reported["task"]["worker_done_at_ms"].is_u64());
        assert_eq!(
            call(AGENT_TASK_REPORT_METHOD, report, None)["code"],
            "worker_done_already_reported"
        );
        let reopened =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&path).unwrap())));
        assert_eq!(
            reopened.task_status(&task.task_id)["task"]["blocked_on_gate"],
            true
        );
        {
            // Exercise the same durable compare-and-set that the separately
            // tested terminal boundary uses, through a second SQLite handle.
            let store = StateStore::open(&path).unwrap();
            assert_eq!(
                store
                    .close_decision_gate(gate_id, Some("invalid"), 100)
                    .unwrap_err(),
                "gate_invalid_resolution"
            );
            assert_eq!(
                store.decision_gate(gate_id).unwrap().unwrap().status,
                "open"
            );
            let resolved = store
                .close_decision_gate(gate_id, Some("continue"), 101)
                .unwrap();
            assert_eq!(
                resolved.resolved_by,
                Some(json!({"class": "local_cli_interactive"}))
            );
            assert_eq!(
                store
                    .close_decision_gate(gate_id, Some("continue"), 102)
                    .unwrap_err(),
                "gate_not_open"
            );
        }
        let completed = reopened.task_status(&task.task_id);
        assert_eq!(completed["task"]["terminal"], true);
        assert_eq!(completed["task"]["terminal_status"], "worker_done");
        assert_ne!(completed["task"]["blocked_on_gate"], true);
        assert_eq!(reopened.task_ack(&task.task_id)["ok"], true);
        drop(reopened);
        drop(registry);
        remove_escalation_db(&path);
    }

    /// user parent | Given tasks and bounded choices | When invalid gates,
    /// concurrent creates, cancellation and lifecycle settle are attempted |
    /// Then validation is fail-closed and at most one open gate survives.
    #[test]
    fn decision_gate_validates_bounds_cancels_and_holds_lifecycle_settle() {
        use decision_gates::{CANCEL, CREATE};
        let (path, registry, _) = worker_registry("gate-bounds");
        let task = escalation_task("gate-bounds", "child", PromptTaskLifecycle::Running, 0);
        registry.save_task(&task, None).unwrap();
        let valid =
            json!({"task_id": task.task_id, "question": "Choose", "options": ["yes", "no"]});
        let create = |params| task_call_with_parent_session(&registry, CREATE, &params, None);
        for (key, value) in [
            ("question", json!("界".repeat(342))),
            ("question", json!("")),
            ("question", json!("escape\u{001b}")),
            ("options", json!(["one"])),
            ("options", json!(["same", "same"])),
            ("options", json!(["", "two"])),
            ("options", json!(["界".repeat(43), "two"])),
            (
                "options",
                json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
            ),
        ] {
            let mut params = valid.clone();
            params[key] = value;
            assert_eq!(create(params)["code"], "invalid_params");
        }
        let mut missing = valid.clone();
        missing["task_id"] = json!("missing");
        assert_eq!(create(missing)["code"], "agent_task_not_found");
        let workers = (0..8)
            .map(|_| {
                let registry = registry.clone();
                let params = valid.clone();
                thread::spawn(move || {
                    task_call_with_parent_session(&registry, CREATE, &params, None)
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
        let gate_id =
            results.iter().find(|result| result["ok"] == true).unwrap()["gate"]["gate_id"].clone();
        let mut held = registry.load_task(&task.task_id).unwrap().unwrap();
        let settle = json!({"cursor": 1, "pane_id": "w1:p1", "agent_status": "done"});
        assert!(
            !registry
                .apply_task_window(&mut held, 1, std::slice::from_ref(&settle))
                .unwrap()
        );
        assert_eq!(held.lifecycle, PromptTaskLifecycle::Running);
        assert_eq!(held.last_cursor, 0);
        registry.save_task(&held, None).unwrap();
        assert_eq!(
            registry.task_status(&task.task_id)["task"]["terminal"],
            false
        );
        assert_eq!(
            registry.task_ack(&task.task_id)["code"],
            "task_blocked_on_gate"
        );
        let cancelled =
            task_call_with_parent_session(&registry, CANCEL, &json!({"gate_id": gate_id}), None);
        assert_eq!(cancelled["gate"]["status"], "cancelled");
        assert_eq!(cancelled["gate"]["resolved_by"], Value::Null);
        let mut released = registry.load_task(&task.task_id).unwrap().unwrap();
        assert!(
            registry
                .apply_task_window(&mut released, 1, &[settle])
                .unwrap()
        );
        registry.save_task(&released, None).unwrap();
        assert_eq!(
            registry.task_status(&task.task_id)["task"]["terminal"],
            true
        );
        assert_eq!(create(valid)["code"], "agent_task_terminal");
        drop(registry);
        remove_escalation_db(&path);
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
            assert_eq!(result["wait"]["still_running"], true);
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
        assert_eq!(inbox["wait"]["still_running"], true);
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

    /// user parent session | Given a session-scoped running child task | When
    /// the child reports worker_done | Then a session inbox wait wakes inside
    /// the poll budget and the row carries the receipt vocabulary.
    #[test]
    fn worker_done_wakes_session_scoped_inbox_wait_with_receipt_linkage() {
        let (db_path, registry) = task_registry("wait-session");
        let mut task = escalation_task(
            "dispatch:prompt:session-child",
            "child",
            PromptTaskLifecycle::Running,
            0,
        );
        task.parent_session_ref = Some("session:web".to_owned());
        task.request_id = Some("edge-req-99".to_owned());
        registry.save_task(&task, None).unwrap();

        let reporter = registry.clone();
        let report = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            reporter.task_report(&json!({
                "task_id": "dispatch:prompt:session-child",
                "dispatch_id": "dispatch:prompt:session-child",
                "kind": "worker_done",
            }))
        });

        let started = std::time::Instant::now();
        let inbox = task_call_with_parent_session(
            &registry,
            AGENT_TASK_INBOX_METHOD,
            &json!({"parent_session_ref": "session:web", "wait_ms": 5_000}),
            Some("session:web"),
        );
        let elapsed = started.elapsed();
        report.join().unwrap();

        assert_eq!(inbox["wait"]["woke"], "terminal_task", "{inbox}");
        assert_eq!(inbox["wait"]["still_running"], false);
        assert!(
            elapsed < Duration::from_secs(1),
            "worker_done must wake the parent's session inbox promptly: {elapsed:?}"
        );
        let row = &inbox["tasks"][0];
        assert_eq!(row["task_id"], "dispatch:prompt:session-child");
        assert_eq!(row["receipt_id"], "dispatch:prompt:session-child");
        assert_eq!(row["request_id"], "edge-req-99");
        assert_eq!(row["parent_session_ref"], "session:web");
        assert_eq!(row["terminal_status"], "worker_done");

        drop(registry);
        remove_task_db(&db_path);
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
                "receipt_id",
                "steering_state",
                "superseded_by",
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                requests.lock().unwrap().push((
                    request["method"].as_str().unwrap_or_default().to_owned(),
                    request["params"].clone(),
                ));
                response["id"] = request["id"].clone();
                if !write_fake_reply(&mut stream, &response.to_string()) {
                    break;
                }
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
            requested_target: None,
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
            request_id: None,
            max_runtime_ms: None,
            turn_seq: None,
            on_budget: None,
            budget_events: Vec::new(),
            gate: None,
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
            repair_attempts: 0,
            repair_exhausted_at_ms: None,
            steering_state: None,
            superseded_by: None,
            foreground_blocker: None,
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut line = String::new();
                match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if fake_peer_gone(&error) => break,
                    Err(error) => panic!("fake Herdr socket could not read a request: {error}"),
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
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
                if !write_fake_reply(&mut stream, &response.to_string()) {
                    break;
                }
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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
            kind: None,
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
                requested_target: None,
                fingerprint: task_id,
                idempotency_key: None,
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor,
                parent_target,
                parent_session_ref,
                stall_after_ms: None,
                request_id: None,
                max_runtime_ms: None,
                on_budget: None,
                foreground_blocker: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(task.lifecycle, PromptTaskLifecycle::Running);
        task
    }
    /// Registers one still-pending task (submitted but the agent has not
    /// entered `working` yet) through the real task path.
    fn register_pending_task(
        registry: &PromptRegistry,
        task_id: &str,
        target: &str,
        pane_id: &str,
        start_cursor: u64,
    ) -> PromptTaskRecord {
        let before = task_agent_state(pane_id, "idle", 1);
        let after = task_agent_state(pane_id, "idle", 1);
        let task = registry
            .register_task(RegisterTaskInput {
                task_id,
                target,
                requested_target: None,
                fingerprint: task_id,
                idempotency_key: None,
                before: Some(&before),
                after: Some(&after),
                submitted: Some(true),
                start_cursor,
                parent_target: None,
                parent_session_ref: None,
                stall_after_ms: None,
                request_id: None,
                max_runtime_ms: None,
                on_budget: None,
                foreground_blocker: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(task.lifecycle, PromptTaskLifecycle::AwaitingActivity);
        task
    }

    /// user coordinator | Given a still-pending steering on one agent | When
    /// a second prompt targets the same pane | Then the earlier record is
    /// `superseded` with `superseded_by` naming the newer task, the newer task
    /// keeps its own stage, and `agent.task.status` reads both facts back.
    #[test]
    fn consecutive_prompts_supersede_unacknowledged_task() {
        let (db_path, registry) = task_registry("supersede-pending");
        let first_id = "dispatch:prompt:first";
        let second_id = "dispatch:prompt:second";
        register_pending_task(&registry, first_id, "child", "w1:p1", 10);

        let second = register_pending_task(&registry, second_id, "child", "w1:p1", 20);

        let status = registry.task_status(first_id);
        assert_eq!(status["ok"], true, "{status}");
        assert_eq!(status["task"]["steering_state"], "superseded");
        assert_eq!(status["task"]["superseded_by"], second_id);
        assert_eq!(status["task"]["lifecycle"], "awaiting_activity");
        assert_eq!(status["task"]["terminal"], false);

        let second_status = registry.task_status(second_id);
        assert_eq!(second_status["task"]["steering_state"], "delivered");
        assert_eq!(second_status["task"]["superseded_by"], Value::Null);
        assert_eq!(second.task_id, second_id);

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user coordinator | Given an already-running steering on one agent |
    /// When a second prompt targets the same pane | Then the acknowledged
    /// record is left untouched: a new prompt cannot retroactively unsend
    /// work the agent already picked up.
    #[test]
    fn acknowledged_task_is_not_superseded() {
        let (db_path, registry) = task_registry("supersede-acknowledged");
        let running_id = "dispatch:prompt:acknowledged";
        register_running_task(&registry, running_id, "child", "w1:p1", None, None, 10);

        register_pending_task(&registry, "dispatch:prompt:later", "child", "w1:p1", 20);

        let status = registry.task_status(running_id);
        assert_eq!(status["task"]["steering_state"], "acknowledged");
        assert_eq!(status["task"]["superseded_by"], Value::Null);

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user coordinator | Given a steering superseded on one registry | When
    /// the store is reopened (runtime restart) | Then `agent.task.status`
    /// still reports `superseded` and the original `superseded_by` id.
    #[test]
    fn superseded_state_survives_restart() {
        let (db_path, registry) = task_registry("supersede-restart");
        let first_id = "dispatch:prompt:restart-first";
        let second_id = "dispatch:prompt:restart-second";
        register_pending_task(&registry, first_id, "child", "w1:p1", 10);
        register_pending_task(&registry, second_id, "child", "w1:p1", 20);
        drop(registry);

        let reopened =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(&db_path).unwrap())));
        let status = reopened.task_status(first_id);
        assert_eq!(status["task"]["steering_state"], "superseded");
        assert_eq!(status["task"]["superseded_by"], second_id);

        drop(reopened);
        remove_task_db(&db_path);
    }

    /// user coordinator | Given a superseded task that the agent picks up
    /// anyway | When its own turn event arrives | Then its stage advances to
    /// the real outcome while `superseded_by` keeps the audit fact.
    #[test]
    fn superseded_task_reports_its_own_outcome() {
        let (db_path, registry) = task_registry("supersede-outcome");
        let first_id = "dispatch:prompt:outcome-first";
        let second_id = "dispatch:prompt:outcome-second";
        register_pending_task(&registry, first_id, "child", "w1:p1", 10);
        register_pending_task(&registry, second_id, "child", "w1:p1", 20);

        let mut task = registry.load_task(first_id).unwrap().unwrap();
        assert!(apply_task_event(
            &mut task,
            &json!({
                "type": "pane.agent_status_changed", "pane_id": "w1:p1",
                "agent_status": "working", "cursor": 30,
            })
        ));
        update_steering(&mut task);
        registry.save_task(&task, None).unwrap();

        let status = registry.task_status(first_id);
        assert_eq!(status["task"]["steering_state"], "acknowledged");
        assert_eq!(status["task"]["superseded_by"], second_id);

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user coordinator | Given a durable record written before B-1 existed |
    /// When it is decoded and projected | Then steering fields are absent on
    /// disk but `agent.task.status` still reports the derived stage, and a
    /// B-1 record survives an older reader that ignores the new fields.
    #[test]
    fn steering_fields_are_rollback_safe() {
        let base = excerpt_test_task("dispatch:prompt:steering-serde");
        assert_eq!(base.steering_state, None);
        assert_eq!(base.superseded_by, None);
        let old_shape = serde_json::to_string(&base).unwrap();
        assert!(!old_shape.contains("steering_state"));
        assert!(!old_shape.contains("superseded_by"));
        let decoded: PromptTaskRecord = serde_json::from_str(&old_shape).unwrap();
        assert_eq!(decoded.steering_state, None);
        // Legacy row: public projection derives the stage from lifecycle.
        let public = decoded.public_json_at(2);
        assert_eq!(public["steering_state"], "acknowledged");
        assert_eq!(public["superseded_by"], Value::Null);

        // A B-1 row stays decodable for an older schema: unknown keys are
        // ignored, matching the final_excerpt rollback rule.
        let upgraded = serde_json::to_string(&base).unwrap().replace(
            '}',
            ",\"steering_state\":\"superseded\",\"superseded_by\":\"dispatch:prompt:newer\"}",
        );
        let decoded: PromptTaskRecord = serde_json::from_str(&upgraded).unwrap();
        assert_eq!(decoded.steering_state.as_deref(), Some("superseded"));
        assert_eq!(
            decoded.superseded_by.as_deref(),
            Some("dispatch:prompt:newer")
        );
    }

    /// user coordinator | Given a dispatch whose agent stays idle because a
    /// foreground program owns the pane | When the task is registered | Then
    /// the durable record carries a sanitized `foreground_blocker` summary,
    /// `steering_state` stays `delivered`, and `agent.task.status` reads the
    /// blocker back with no raw argv.
    #[test]
    fn foreground_blocker_is_recorded_while_steering_stays_delivered() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let texts = Arc::new(Mutex::new(Vec::new()));
        let server = scripted_dispatch_server(
            listener,
            vec![
                json!({"result": {"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1, "kind": "pi"}}}),
                json!({"result": {"prompt": {"status": "submitted", "agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1, "kind": "pi"}}}}),
                json!({"result": {"process_info": {
                    "shell_pid": 42,
                    "foreground_process_group_id": 77,
                    "foreground_processes": [
                        {"pid": 77, "name": "less", "command": "less -S /var/log/syslog --token abc123 KEY=secret"},
                        {"pid": 90, "name": "pi"}
                    ],
                }}}),
            ],
            texts.clone(),
        );
        let client = HerdrClient::new(&socket);
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let registry = PromptRegistry::with_store(store);
        let result = run(
            &client,
            &registry,
            &json!({"target": "w1:p1", "text": "hello\n", "idempotency_key": "blocker-e2e"}),
        );
        assert_eq!(result["ok"], true, "{result}");
        server.join().unwrap();

        let task_id = result["task_id"].as_str().unwrap();
        let status = registry.task_status(task_id);
        assert_eq!(status["task"]["steering_state"], "delivered", "{status}");
        assert_eq!(status["task"]["terminal"], false);
        let blocker = &status["task"]["foreground_blocker"];
        assert_eq!(blocker["pane_id"], "w1:p1", "{blocker}");
        assert_eq!(blocker["source"], "pane.process_info");
        assert!(blocker["observed_at_ms"].as_u64().unwrap() > 0);
        let processes = blocker["processes"].as_array().unwrap();
        let less = processes.iter().find(|p| p["name"] == "less").unwrap();
        assert_eq!(less["pid"], 77);
        assert_eq!(less["agent"], Value::Null);
        // The argv tail is cut at the first credential flag; the earlier
        // allowlisted words survive, nothing secret does.
        assert_eq!(less["command_prefix"], "less -S varlogsyslog");
        let pi = processes.iter().find(|p| p["name"] == "pi").unwrap();
        assert_eq!(pi["agent"], true);
        assert_eq!(blocker["non_shell_processes"], 1);
        // The serialized record must not contain the raw argv anywhere.
        let persisted =
            serde_json::to_string(&registry.load_task(task_id).unwrap().unwrap()).unwrap();
        for leak in ["abc123", "KEY=secret", "--token"] {
            assert!(!persisted.contains(leak), "leaked {leak}");
        }
        fs::remove_file(socket).unwrap();
    }

    /// user coordinator | Given a blocked prompt whose agent later picks it
    /// up | When its own working event lands | Then `steering_state` advances
    /// to acknowledged and the blocker fact clears: status never claims a
    /// running prompt is still blocked.
    #[test]
    fn foreground_blocker_clears_on_agent_pickup() {
        let (db_path, registry) = task_registry("blocker-clears");
        let task =
            register_pending_task(&registry, "dispatch:prompt:blocked", "child", "w1:p1", 10);
        let mut persisted = registry.load_task(&task.task_id).unwrap().unwrap();
        persisted.foreground_blocker = Some(json!({
            "pane_id": "w1:p1", "observed_at_ms": 5,
            "processes": [{"pid": 77, "name": "less"}],
        }));
        registry.save_task(&persisted, None).unwrap();

        let mut task = persisted;
        assert!(apply_task_event(
            &mut task,
            &json!({
                "type": "pane.agent_status_changed", "pane_id": "w1:p1",
                "agent_status": "working", "cursor": 30,
            })
        ));
        registry.save_task(&task, None).unwrap();

        let status = registry.task_status(&task.task_id);
        assert_eq!(status["task"]["steering_state"], "acknowledged");
        assert_eq!(status["task"]["foreground_blocker"], Value::Null);

        drop(registry);
        remove_task_db(&db_path);
    }

    /// user coordinator | Given argv that mixes safe words with credentials |
    /// When the blocker summary is shaped | Then only the allowlisted prefix
    /// survives and no secret, flag value, or env assignment can leak.
    #[test]
    fn foreground_blocker_redacts_credentials_and_env() {
        let prefix = sanitize_command_prefix("git push --force origin main").unwrap();
        assert_eq!(prefix, "git push --force origin main");

        for secret_case in [
            "deploy --token abc123",
            "deploy --api-key=abc123",
            "deploy PASS=supersecret",
            "curl -H Bearer abc123",
            "tool --password pw",
            "tool API_KEY=abc def",
        ] {
            let shaped = sanitize_command_prefix(secret_case).unwrap_or_default();
            assert!(!shaped.contains("abc123"), "{secret_case} -> {shaped}");
            assert!(!shaped.contains("supersecret"), "{secret_case} -> {shaped}");
            assert!(!shaped.contains("pw"), "{secret_case} -> {shaped}");
            assert!(!shaped.contains("def"), "{secret_case} -> {shaped}");
            assert!(
                shaped.starts_with("deploy")
                    || shaped.starts_with("curl")
                    || shaped.starts_with("tool")
            );
        }

        // A token that looks like a path or an env assignment ends the prefix.
        assert_eq!(
            sanitize_command_prefix("less /etc/passwd").unwrap(),
            "less etcpasswd"
        );
        assert_eq!(sanitize_command_prefix("env FOO=bar baz").unwrap(), "env");

        // Process entries keep pid + sanitized name only when argv is absent.
        let entry =
            sanitize_foreground_process(&json!({"pid": 77, "name": "  less\n"}), Some(42), None)
                .unwrap();
        assert_eq!(entry["pid"], 77);
        assert_eq!(entry["name"], "less");
        assert_eq!(entry["command_prefix"], Value::Null);
    }

    /// user coordinator | Given foreground reports for shell-only, agent-own
    /// and genuinely blocked panes | When the blocker summary is shaped |
    /// Then shell-only and agent-only foregrounds yield no blocker while a
    /// foreign program does.
    #[test]
    fn foreground_blocker_skips_shell_and_agent_foreground() {
        // Agent-owned foreground only: the idle agent pane itself, no blocker.
        let entry =
            sanitize_foreground_process(&json!({"pid": 77, "name": "omp"}), Some(42), Some("omp"))
                .unwrap();
        assert_eq!(entry["agent"], true);
        let entry =
            sanitize_foreground_process(&json!({"pid": 78, "name": "less"}), Some(42), Some("omp"))
                .unwrap();
        assert_eq!(entry["agent"], Value::Null);

        // Kind matching is basename- and case-exact to the reported kind.
        assert!(agent_process_names_match("pi", "PI"));
        assert!(!agent_process_names_match("pi", "pip"));
        assert!(!agent_process_names_match("", "pi"));
        assert!(!agent_process_names_match("pi", ""));
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
                role_dispatches: Mutex::new(()),
                spawn_resumes: Mutex::new(HashSet::new()),
                spawn_early_exit_watch_disabled: AtomicBool::new(false),
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

    // ---- 1.2 S5: model alias validation, early-exit detection, run bounds ----

    /// Write a fake `pi` whose `--list-models` is the authoritative registry
    /// (S5 review P1-1). Caller holds test_env::lock and restores.
    fn s5_pi_registry(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "herdr-mcp-s5-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("pi");
        fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = \"--list-models\" ]; then\ncat <<'EOF'\nprovider                model\nopenai                  gpt-4\nopenai                  gpt-5.1-codex\nmagpie                  aliyun/auto/deepseek-v4.1-flash\nanthropic               claude-opus-4-5\nEOF\nelse\nexit 1\nfi\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    /// The fake schema document injected on-thread for model-delivery tests:
    /// agent.start advertises an `args` passthrough (the live shape today).
    /// Thread-local injection keeps these tests independent of the shared
    /// 60-second schema cache (S5 review P1-3).
    fn s5_schema_with_args() -> serde_json::Value {
        json!({"schemas":{"request":{"oneOf":[{"properties":{"method":{"const":"agent.start"},"params":{"type":"object","properties":{"name":{"type":"string"},"kind":{"type":"string"},"pane_id":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}},"required":["name","kind","pane_id"]}}}],"$defs":{}}}})
    }

    struct S5Env {
        prev_pi: Option<std::ffi::OsString>,
        prev_pi_bin: Option<std::ffi::OsString>,
        dirs: Vec<PathBuf>,
    }

    impl S5Env {
        fn set(tag: &str) -> Self {
            let prev_pi = env::var_os("PI_CODING_AGENT_DIR");
            let prev_pi_bin = env::var_os("HERDR_PI_BIN");
            let registry_dir = s5_pi_registry(tag);
            unsafe {
                env::set_var("HERDR_PI_BIN", registry_dir.join("pi"));
                env::remove_var("PI_CODING_AGENT_DIR");
            }
            Self {
                prev_pi,
                prev_pi_bin,
                dirs: vec![registry_dir],
            }
        }
    }

    impl Drop for S5Env {
        fn drop(&mut self) {
            unsafe {
                match self.prev_pi.take() {
                    Some(value) => env::set_var("PI_CODING_AGENT_DIR", value),
                    None => env::remove_var("PI_CODING_AGENT_DIR"),
                }
                match self.prev_pi_bin.take() {
                    Some(value) => env::set_var("HERDR_PI_BIN", value),
                    None => env::remove_var("HERDR_PI_BIN"),
                }
            }
            for dir in &self.dirs {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    /// user planner | Given a spawn with an unknown model alias | When the
    /// dispatch is admitted | Then it fails at validate with the available
    /// list, creates nothing, and a replay returns the same refusal.
    #[test]
    fn spawn_unknown_model_alias_fails_before_any_mutation() {
        let root = spawn_project_root("s5-unknown");
        let socket = temp_socket();
        let listener = refused_spawn_server(&socket);
        // The watch-off guard owns the crate env lock for its lifetime; all
        // other env mutations in this test run after it is taken.
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let _env = S5Env::set("unknown-alias");
        let args = json!({
            "spawn": {"kind": "pi", "workspace_id": "w1", "model": "openai/gpt-99"},
            "text": "work",
            "idempotency_key": "s5-unknown",
        });
        let client = HerdrClient::new(&socket);
        let first = run(&client, &registry, &args);
        assert_eq!(first["ok"], false, "{first}");
        assert_eq!(first["failed_step"], "validate");
        assert_eq!(first["code"], "model_alias_unknown");
        assert_eq!(first["delivery_state"], "not_delivered");
        assert_eq!(first["fallback_allowed"], false);
        assert_eq!(first["model_validation"]["validated"], false);
        assert_eq!(
            first["available_models"],
            json!([
                "openai/gpt-4",
                "openai/gpt-5.1-codex",
                "magpie/aliyun/auto/deepseek-v4.1-flash",
                "anthropic/claude-opus-4-5",
            ])
        );
        // Validation happens before key reservation (same as every spawn
        // validate failure): a retry re-validates and refuses identically,
        // still without any mutation.
        let replay = run(&client, &registry, &args);
        assert_eq!(replay["code"], "model_alias_unknown");
        assert_eq!(replay["failed_step"], "validate");
        assert!(registry.list_tasks(10).unwrap().is_empty());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a spawn kind without an obtainable model list |
    /// When a model is requested | Then validated:false and no spawn happens.
    #[test]
    fn spawn_model_on_kind_without_list_is_unvalidatable() {
        let root = spawn_project_root("s5-nolist");
        let socket = temp_socket();
        let listener = refused_spawn_server(&socket);
        // The watch-off guard owns the crate env lock for its lifetime.
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let _env = S5Env::set("no-list");
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "claude", "workspace_id": "w1", "model": "sonnet"},
                "text": "work",
                "idempotency_key": "s5-nolist",
            }),
        );
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "model_alias_unknown");
        assert_eq!(result["model_validation"]["validated"], false);
        assert_eq!(result["available_models"], json!([]));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a validated pi alias and a live schema with args |
    /// When the spawn starts the agent | Then --model travels in agent.start
    /// args and the prompt is submitted exactly once.
    #[test]
    fn spawn_validated_model_delivers_model_flag() {
        let root = spawn_project_root("s5-modelflag");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("tab.create", spawn_tab_created()["result"].clone()),
                ("agent.start", spawn_agent_started(true)["result"].clone()),
                (
                    "agent.get",
                    json!({"agent": {"pane_id": "w1:p9", "agent_status": "idle", "interactive_ready": true, "state_change_seq": 1}}),
                ),
                ("agent.prompt", spawn_prompt_submitted()["result"].clone()),
            ],
            requests.clone(),
        );
        // The watch-off guard owns the crate env lock for its lifetime.
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let _env = S5Env::set("model-flag");
        let result = crate::schema::with_test_schema_document(s5_schema_with_args(), || {
            run(
                &HerdrClient::new(&socket),
                &registry,
                &json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1", "model": "openai/gpt-4", "startup_timeout_ms": 40_000},
                    "text": "work",
                    "idempotency_key": "s5-modelflag",
                }),
            )
        });
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        let recorded = requests.lock().unwrap();
        let start = recorded
            .iter()
            .find(|request| request["method"] == "agent.start")
            .expect("agent.start must be called");
        assert_eq!(start["params"]["args"], json!(["--model", "openai/gpt-4"]));
        assert!(start["params"].get("model").is_none());
        assert_eq!(
            recorded
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count(),
            1
        );
        drop(recorded);
        // S5 review P1-2: a replay under the same key must return the
        // recorded outcome without re-running live admission. The fake
        // server is already stopped and the PI env points nowhere, so any
        // probe would fail; the replay must still answer ok:true.
        let replay = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1", "model": "openai/gpt-4"},
                "text": "work",
                "idempotency_key": "s5-modelflag",
            }),
        );
        assert_eq!(replay["ok"], true, "{replay}");
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "agent.prompt")
                .count(),
            1,
            "a replay must never re-prompt"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given the live schema cannot carry a model | When a
    /// validated alias is spawned | Then model_delivery_unsupported before
    /// agent.start, never a silent drop.
    #[test]
    fn spawn_model_without_schema_delivery_fails_closed() {
        let root = spawn_project_root("s5-nodelivery");
        let socket = temp_socket();
        // The refusal now happens in parse_spawn, before any socket contact:
        // no pane is ever created for a schema that cannot carry a model.
        let listener = refused_spawn_server(&socket);
        // The watch-off guard owns the crate env lock for its lifetime; the
        // HERDR_BIN / catalog overrides below run after it is taken.
        let (registry, _watch_off) = spawn_registry(&root, 80);
        // Force the schema unavailable **on this thread** instead of relying
        // on a failing HERDR_BIN plus the process-level schema cache: the cache
        // is shared (TTL 60s), so an earlier test's fake herdr could answer for
        // this one and the refusal would never be reached (S5 review P1-3).
        let _env = S5Env::set("no-delivery");
        let result = crate::schema::with_test_schema_unavailable(|| {
            run(
                &HerdrClient::new(&socket),
                &registry,
                &json!({
                    "spawn": {"kind": "pi", "workspace_id": "w1", "model": "openai/gpt-4"},
                    "text": "work",
                    "idempotency_key": "s5-nodelivery",
                }),
            )
        });
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "model_delivery_unsupported");
        assert_eq!(result["failed_step"], "validate");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["model_validation"]["validated"], true);
        assert!(result.get("reclaim").is_none(), "{result}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a started agent that dies before readiness | When
    /// the spawn chain probes it | Then agent_start_failed with a redacted
    /// pane tail, no prompt, and the pane kept for reclaim.
    #[test]
    fn spawn_dead_on_start_reports_agent_start_failed_with_redacted_tail() {
        let root = spawn_project_root("s5-deadstart");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("tab.create", spawn_tab_created()["result"].clone()),
                ("agent.start", spawn_agent_started(false)["result"].clone()),
                (
                    "agent.get",
                    json!({"error": {"code": "agent_not_found", "message": "no agent"}}),
                ),
                (
                    "pane.read",
                    json!({"read": {"content": "model `gpt-99` not found\napi_key=sk-secret-1234567890abcdef\n$ "}}),
                ),
            ],
            requests.clone(),
        );
        let (registry, _watch_off) = spawn_registry(&root, 80);
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1"},
                "text": "work",
                "idempotency_key": "s5-deadstart",
            }),
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["failed_step"], "agent_start");
        assert_eq!(result["code"], "agent_start_failed");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["submitted"], false);
        assert_eq!(result["spawn"]["pane_id"], "w1:p9");
        assert_eq!(result["reclaim"]["pane_id"], "w1:p9");
        let tail = result["pane_tail"].as_str().expect("pane tail");
        assert!(tail.contains("model `gpt-99` not found"), "{tail}");
        assert!(
            !tail.contains("sk-secret-1234567890abcdef"),
            "secret must be redacted: {tail}"
        );
        assert_eq!(result["pane_tail_redacted"], true);
        let recorded = requests.lock().unwrap();
        assert!(
            recorded.iter().all(|r| r["method"] != "agent.prompt"),
            "a dead agent must never be prompted: {recorded:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given an agent that starts ready but exits inside the
    /// early-exit window | When the chain watches it | Then agent_start_failed
    /// and no prompt is submitted.
    #[test]
    fn spawn_early_exit_after_ready_reports_agent_start_failed() {
        // Hold the shared test-env lock for the whole dispatch: spawn_registry
        // helpers briefly set HERDR_MCP_SPAWN_EARLY_EXIT_MS=0, and this test
        // needs the default watch window to stay in effect throughout.
        let _guard = crate::test_env::lock();
        let root = spawn_project_root("s5-earlyexit");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // agent.get answers ready for the first probe (readiness), then reports
        // the agent gone. The watch window is left at its default; the fake
        // answers every probe, so no env override (and no cross-test race) is
        // needed. The chain budget is large enough that the default 10s watch
        // (capped to chain/4) fits with the prompt reserve.
        let get_calls = Arc::new(AtomicU64::new(0));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = {
            let requests = requests.clone();
            let stop = stop.clone();
            let get_calls = get_calls.clone();
            thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                while !stop.load(Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut line = String::new();
                    match BufReader::new(stream.try_clone().unwrap()).read_line(&mut line) {
                        Ok(0) => continue,
                        Ok(_) => {}
                        Err(error) if fake_peer_gone(&error) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                        Err(error) => panic!("fake read failed: {error}"),
                    }
                    let Ok(request) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    requests.lock().unwrap().push(request.clone());
                    let method = request["method"].as_str().unwrap_or_default();
                    let response = match method {
                        "tab.create" => spawn_tab_created()["result"].clone(),
                        "agent.start" => spawn_agent_started(true)["result"].clone(),
                        "agent.get" => {
                            let n = get_calls.fetch_add(1, Ordering::SeqCst);
                            if n == 0 {
                                json!({"agent": {"pane_id": "w1:p9", "agent_status": "idle", "interactive_ready": true, "state_change_seq": 1}})
                            } else {
                                json!({"error": {"code": "agent_not_found", "message": "exited"}})
                            }
                        }
                        "pane.read" => json!({"read": {"content": "fatal: bad model\n$ "}}),
                        _ => json!({"error": {"code": "unscripted_method", "message": method}}),
                    };
                    let envelope = if let Some(error) = response.get("error") {
                        json!({"id": request["id"], "error": error})
                    } else {
                        json!({"id": request["id"], "result": response})
                    };
                    if !write_fake_reply(&mut stream, &envelope.to_string()) {
                        continue;
                    }
                }
            })
        };
        // Build the registry directly (not via spawn_registry, which disables
        // the watch for scripted fakes) so the default watch window applies.
        let mut registry =
            PromptRegistry::with_store(Arc::new(Mutex::new(StateStore::open(":memory:").unwrap())));
        Arc::get_mut(&mut registry.inner).unwrap().cache = Some(Arc::new(
            EventCache::from_snapshot_for_test(spawn_snapshot(&root, 80)),
        ));
        let result = run(
            &HerdrClient::new(&socket),
            &registry,
            &json!({
                "spawn": {"kind": "pi", "workspace_id": "w1", "startup_timeout_ms": 60_000},
                "text": "work",
                "idempotency_key": "s5-earlyexit",
            }),
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "agent_start_failed", "{result}");
        assert_eq!(result["failed_step"], "agent_start");
        assert_eq!(result["delivery_state"], "not_delivered");
        let recorded = requests.lock().unwrap();
        assert!(
            recorded.iter().all(|r| r["method"] != "agent.prompt"),
            "early-exit must prevent the prompt: {recorded:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// user planner | Given a task with an explicit run cap | When the cap
    /// expires | Then one agent_budget_exceeded event notifies the parent,
    /// the task stays non-terminal, and a second sweep never re-notifies.
    #[test]
    fn budget_expiry_notifies_parent_once_without_terminating() {
        let (path, mut registry, clock) = worker_registry("s5-budget");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![("agent.prompt", json!({"prompt": {"status": "submitted"}}))],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-budget-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.max_runtime_ms = Some(60_000);
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        // Before the cap: nothing.
        registry.sweep_task_budgets();
        assert!(
            registry
                .load_task(&task.task_id)
                .unwrap()
                .unwrap()
                .budget_events
                .is_empty()
        );
        // Past the cap: exactly one event, notify-only, task still running.
        clock.fetch_add(61_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert!(!stored.is_terminal());
        assert_eq!(stored.lifecycle, PromptTaskLifecycle::Running);
        assert_eq!(stored.budget_events.len(), 1);
        assert_eq!(stored.budget_events[0]["event"], "agent_budget_exceeded");
        assert_eq!(stored.budget_events[0]["notification_state"], "delivered");
        assert_eq!(stored.budget_events[0]["interrupt_state"], "not_requested");
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["method"], "agent.prompt");
        assert_eq!(recorded[0]["params"]["target"], "w1:p1");
        assert!(
            recorded[0]["params"]["text"]
                .as_str()
                .unwrap()
                .contains("agent_budget_exceeded")
        );
        drop(recorded);
        // A second sweep must not re-notify.
        registry.sweep_task_budgets();
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert_eq!(
            registry
                .load_task(&task.task_id)
                .unwrap()
                .unwrap()
                .budget_events
                .len(),
            1
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user parent | Given a budget event persisted as pending (the process
    /// died between the durable write and delivery) | When the next sweep runs
    /// | Then the pending event is delivered instead of being skipped forever.
    #[test]
    fn pending_budget_event_is_redelivered_after_a_crash_window() {
        let (path, mut registry, clock) = worker_registry("s5-budget-crash");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![("agent.prompt", json!({"prompt": {"status": "submitted"}}))],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-crash-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.max_runtime_ms = Some(60_000);
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        // Simulate the crash window: the event row exists but is still pending.
        task.budget_events.push(json!({
            "event": "agent_budget_exceeded",
            "at_ms": clock.load(Ordering::SeqCst),
            "notification_state": "pending",
            "interrupt_state": "not_requested",
        }));
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(61_000, Ordering::SeqCst);

        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(stored.budget_events.len(), 1, "no duplicate event row");
        assert_eq!(
            stored.budget_events[0]["notification_state"], "delivered",
            "the pending event must be delivered by the sweep"
        );
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0]["method"], "agent.prompt");
        assert_eq!(recorded[0]["params"]["target"], "w1:p1");
        assert!(
            recorded[0]["params"]["text"]
                .as_str()
                .unwrap()
                .contains("agent_budget_exceeded")
        );
        drop(recorded);
        // Delivered rows are not re-sent.
        registry.sweep_task_budgets();
        assert_eq!(requests.lock().unwrap().len(), 1);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user planner | Given on_budget interrupt | When the cap expires while
    /// the agent works | Then one C-c goes to the task's own pane and the
    /// event records it; a second sweep sends nothing more.
    #[test]
    fn budget_interrupt_sends_one_cc_to_the_task_pane() {
        let (path, mut registry, clock) = worker_registry("s5-interrupt");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("agent.prompt", json!({"prompt": {"status": "submitted"}})),
                (
                    "agent.get",
                    json!({"agent": {"pane_id": "w1:p9", "agent_status": "working", "state_change_seq": 3}}),
                ),
                ("pane.send_keys", json!({"ok": true})),
            ],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-int-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.pane_id = Some("w1:p9".to_owned());
        // The task's live turn is pinned to the same seq the pane reports
        // below; the interrupt fence re-checks it at send time (S5 review
        // P1-4).
        task.turn_seq = Some(3);
        task.max_runtime_ms = Some(60_000);
        task.on_budget = Some("interrupt".to_owned());
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(61_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert!(!stored.is_terminal());
        assert_eq!(stored.budget_events.len(), 1);
        assert_eq!(stored.budget_events[0]["interrupt_state"], "sent");
        let recorded = requests.lock().unwrap();
        let cc: Vec<_> = recorded
            .iter()
            .filter(|r| r["method"] == "pane.send_keys")
            .collect();
        assert_eq!(cc.len(), 1, "{recorded:?}");
        assert_eq!(cc[0]["params"]["pane_id"], "w1:p9");
        assert_eq!(cc[0]["params"]["keys"], json!(["C-c"]));
        drop(recorded);
        registry.sweep_task_budgets();
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "pane.send_keys")
                .count(),
            1
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user planner | Given on_budget interrupt and both stall + runtime caps
    /// expiring in one sweep | Then the parent gets both notifications but the
    /// pane receives exactly one C-c (S5 review P1-3).
    #[test]
    fn budget_interrupt_sends_at_most_one_cc_across_two_due_events() {
        let (path, mut registry, clock) = worker_registry("s5-interrupt-once");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("agent.prompt", json!({"prompt": {"status": "submitted"}})),
                (
                    "agent.get",
                    json!({"agent": {"pane_id": "w1:p9", "agent_status": "working", "state_change_seq": 3}}),
                ),
                ("pane.send_keys", json!({"ok": true})),
            ],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-int2-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.pane_id = Some("w1:p9".to_owned());
        task.turn_seq = Some(3);
        task.stall_after_ms = Some(60_000);
        task.max_runtime_ms = Some(60_000);
        task.on_budget = Some("interrupt".to_owned());
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(61_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(stored.budget_events.len(), 2, "{:?}", stored.budget_events);
        let interrupts: Vec<_> = stored
            .budget_events
            .iter()
            .map(|row| row["interrupt_state"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            interrupts.iter().filter(|state| **state == "sent").count(),
            1,
            "exactly one event may send the C-c: {interrupts:?}"
        );
        let cc = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "pane.send_keys")
            .count();
        assert_eq!(cc, 1);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user planner | Given the pane moved past the task's pinned turn |
    /// When the interrupt fires | Then the C-c is refused rather than
    /// interrupting unrelated new work (S5 review P1-4).
    #[test]
    fn budget_interrupt_refuses_when_the_pane_moved_to_a_new_turn() {
        let (path, mut registry, clock) = worker_registry("s5-interrupt-turn");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![
                ("agent.prompt", json!({"prompt": {"status": "submitted"}})),
                (
                    "agent.get",
                    // A later turn than the task's pinned seq 3.
                    json!({"agent": {"pane_id": "w1:p9", "agent_status": "working", "state_change_seq": 7}}),
                ),
                ("pane.send_keys", json!({"ok": true})),
            ],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-int3-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.pane_id = Some("w1:p9".to_owned());
        task.turn_seq = Some(3);
        task.max_runtime_ms = Some(60_000);
        task.on_budget = Some("interrupt".to_owned());
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(61_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(stored.budget_events.len(), 1);
        assert_eq!(stored.budget_events[0]["interrupt_state"], "turn_mismatch");
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["method"] != "pane.send_keys"),
            "no C-c may reach a pane serving a different turn"
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user planner | Given an active task older than a full page of terminal
    /// history | When the budget sweep runs | Then the old task's cap still
    /// emits (S5 review P1-5).
    #[test]
    fn budget_sweep_sees_active_tasks_beyond_the_recent_window() {
        let (path, registry, clock) = worker_registry("s5-sweep-window");
        // An old active task whose cap expired long ago.
        let mut task = escalation_task("s5-old-active", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.max_runtime_ms = Some(60_000);
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        // Fill the recent window with terminal tasks so a bounded updated_at
        // query could never reach the active one.
        for index in 0..520 {
            let terminal = escalation_task(
                &format!("s5-terminal-{index}"),
                "w1:p8",
                PromptTaskLifecycle::Completed,
                0,
            );
            registry.save_task(&terminal, None).unwrap();
        }
        clock.fetch_add(120_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(
            stored.budget_events.len(),
            1,
            "an old active task must still emit its cap: {:?}",
            stored.budget_events
        );
        assert_eq!(stored.budget_events[0]["event"], "agent_budget_exceeded");
        let _ = fs::remove_file(path);
    }

    /// user planner | Given an explicit stall threshold | When it expires |
    /// Then one agent_stalled event notifies the parent and the stall clock
    /// is not reset by the event itself.
    #[test]
    fn explicit_stall_expiry_emits_agent_stalled_once() {
        let (path, mut registry, clock) = worker_registry("s5-stall");
        let socket = temp_socket();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = repeating_spawn_server(
            UnixListener::bind(&socket).unwrap(),
            stop.clone(),
            vec![("agent.prompt", json!({"prompt": {"status": "submitted"}}))],
            requests.clone(),
        );
        Arc::get_mut(&mut registry.inner).unwrap().client = Some(HerdrClient::new(&socket));
        let mut task = escalation_task("s5-stall-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        task.parent_target = Some("w1:p1".to_owned());
        task.stall_after_ms = Some(60_000);
        task.created_at_ms = clock.load(Ordering::SeqCst);
        task.updated_at_ms = clock.load(Ordering::SeqCst);
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(61_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert_eq!(stored.budget_events.len(), 1);
        assert_eq!(stored.budget_events[0]["event"], "agent_stalled");
        assert_eq!(
            stored.updated_at_ms, task.updated_at_ms,
            "stall event must not reset the stall clock"
        );
        assert!(!stored.is_terminal());
        registry.sweep_task_budgets();
        assert_eq!(requests.lock().unwrap().len(), 1);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }

    /// user planner | Given a task without caps | When time passes | Then no
    /// budget event is emitted and behavior is unchanged.
    #[test]
    fn no_caps_means_no_budget_events() {
        let (path, registry, clock) = worker_registry("s5-nocaps");
        let task = escalation_task("s5-nocaps-task", "w1:p9", PromptTaskLifecycle::Running, 0);
        registry.save_task(&task, None).unwrap();
        clock.fetch_add(60 * 60_000, Ordering::SeqCst);
        registry.sweep_task_budgets();
        let stored = registry.load_task(&task.task_id).unwrap().unwrap();
        assert!(stored.budget_events.is_empty());
        assert!(!stored.is_terminal());
        let _ = fs::remove_file(path);
    }

    /// user planner | Given invalid budget params | When dispatch parses them |
    /// Then invalid_params before any mutation.
    #[test]
    fn budget_params_are_validated() {
        let socket = temp_socket();
        let listener = refused_spawn_server(&socket);
        let registry = PromptRegistry::new();
        let client = HerdrClient::new(&socket);
        for (key, value) in [
            ("max_runtime_ms", json!(1000)),
            ("max_runtime_ms", json!(86_400_001u64)),
            ("max_runtime_ms", json!("soon")),
            ("on_budget", json!("kill")),
        ] {
            let mut args = json!({"target": "w1:p1", "text": "work"});
            args[key] = value;
            let result = run(&client, &registry, &args);
            assert_eq!(result["code"], "invalid_params", "{key}: {result}");
        }
        // on_budget without any cap is rejected.
        let result = run(
            &client,
            &registry,
            &json!({"target": "w1:p1", "text": "work", "on_budget": "interrupt"}),
        );
        assert_eq!(result["code"], "invalid_params");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
