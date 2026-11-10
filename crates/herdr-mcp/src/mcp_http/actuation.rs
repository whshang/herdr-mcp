//! Browser actuation broker: the pending-command queue, extension liveness,
//! dispatch reconciliation, and timeout settlement for browser mutations.
//!
//! This module owns only the in-memory actuation handshake between the runtime
//! and the trusted Extension SSE stream. HTTP route admission, trusted-IPC
//! gating, Push/SSE composition, and the caller-grant/generation fences stay in
//! the parent `mcp_http` module.

use crate::mcp::{BrowserActuator, BrowserPostconditionEvidence};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

// The 22s wait covers ordinary browser actuation inside Edge's 30s default
// tool-request deadline (above the 15s extension heartbeat, with room for
// HTTP/serialization overhead). The session-create wait instead bounds the
// extension's full create worst case: the 30s create-scope content-script
// handshake plus the 43s content acceptance leg. It deliberately exceeds the
// Edge/Link public request budgets; a create that settles after the caller's
// budget returns `uncertain` and is reconciled by same-key replay (readback),
// never re-dispatched. Late completions arriving after this wait stay
// retained for BROWSER_LATE_COMPLETION_TTL so the replay can promote them.
const BROWSER_ACTUATION_TIMEOUT: Duration = Duration::from_secs(22);
const BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT: Duration = Duration::from_secs(80);
const BROWSER_LATE_COMPLETION_TTL: Duration = Duration::from_secs(60);
const BROWSER_ACTUATION_ENDPOINT_PARAM: &str = "__herdr_browser_endpoint_ref";
// The extension polls for browser actuation on the shared SSE heartbeat. Keep
// the liveness window above that 15s cadence so an idle healthy stream is not
// classified offline between polls.
const BROWSER_EXTENSION_LIVE_WINDOW: Duration = Duration::from_secs(20);
// #629: how long one endpoint's last-poll timestamp is kept after it stops
// polling. It is deliberately much longer than the liveness window: the
// last-seen age is the diagnostic that separates a closed browser from an
// endpoint that never polled, and pruning it at the liveness window would
// report a known-but-quiet endpoint as never seen.
const BROWSER_ENDPOINT_LAST_SEEN_RETENTION: Duration = Duration::from_secs(600);

static NEXT_BROWSER_ACTUATION: AtomicU64 = AtomicU64::new(0);

/// #629: one content-free structured line per browser-endpoint lifecycle
/// event. Callers pass only opaque ids, enum codes, timestamps, durations and
/// millisecond ages: page content, tab titles, URLs (and their query strings),
/// `profile_seed` values and credentials must never reach this formatter.
fn browser_endpoint_log_line(event: &str, fields: &[(&str, String)]) -> String {
    let mut line = format!("herdr-mcp: browser endpoint event={event}");
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&sanitize_log_field(value));
    }
    line
}

/// Keep one lifecycle event on one greppable line: no control characters, no
/// embedded whitespace that would split a field, and a bounded width. The bound
/// is wider than the longest validated id (`endpoint_ref` is at most 96 bytes),
/// so an id is never truncated into an unmatchable prefix.
fn sanitize_log_field(value: &str) -> String {
    const MAX_LOG_FIELD_CHARS: usize = 128;
    value
        .chars()
        .take(MAX_LOG_FIELD_CHARS)
        .map(|character| {
            if character.is_control() || character.is_whitespace() {
                '_'
            } else {
                character
            }
        })
        .collect()
}

/// Emit a lifecycle event through the runtime's existing logging facility
/// (stderr, captured into `server.log` by the managed service owner). There is
/// no separate log file, rotation policy or tracing crate in this runtime, and
/// #629 deliberately does not add one.
pub(super) fn emit_browser_endpoint_event(event: &str, fields: &[(&str, String)]) {
    let line = browser_endpoint_log_line(event, fields);
    #[cfg(test)]
    endpoint_log_capture::record(&line);
    eprintln!("{line}");
}

/// Test-only sink so a test can assert what a lifecycle line actually contains
/// without redirecting the process stderr.
#[cfg(test)]
pub(super) mod endpoint_log_capture {
    use std::sync::Mutex;

    static CAPTURED_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub(super) fn record(line: &str) {
        if let Ok(mut lines) = CAPTURED_LINES.lock() {
            lines.push(line.to_owned());
            let excess = lines.len().saturating_sub(256);
            lines.drain(..excess);
        }
    }

    /// Drain the captured lines for one endpoint. Tests use a unique endpoint
    /// ref so parallel tests cannot consume each other's lines.
    pub(in crate::mcp_http) fn take_for_endpoint(endpoint_ref: &str) -> Vec<String> {
        let Ok(mut lines) = CAPTURED_LINES.lock() else {
            return Vec::new();
        };
        let needle = format!("endpoint_ref={endpoint_ref} ");
        let exact = format!("endpoint_ref={endpoint_ref}");
        let mut matched = Vec::new();
        let mut kept = Vec::new();
        for line in lines.drain(..) {
            if line.contains(&needle) || line.ends_with(&exact) {
                matched.push(line);
            } else {
                kept.push(line);
            }
        }
        *lines = kept;
        matched
    }
}

fn log_endpoint_ref(endpoint_ref: Option<&str>) -> String {
    endpoint_ref.unwrap_or("-").to_owned()
}

fn log_last_seen_ms(last_seen_age_ms: Option<u64>) -> String {
    last_seen_age_ms
        .map(|age| age.to_string())
        .unwrap_or_else(|| "never".to_owned())
}

#[derive(Clone)]
pub(super) struct BrowserActuationBroker {
    inner: Arc<(Mutex<BrowserActuationState>, Condvar)>,
    wake_tx: tokio::sync::watch::Sender<u64>,
    timeout: Duration,
    create_timeout: Duration,
    late_completion_ttl: Duration,
}

#[derive(Default)]
struct BrowserActuationState {
    queued: VecDeque<Value>,
    pending: HashMap<String, PendingBrowserActuation>,
    completions: HashMap<String, BrowserPostconditionEvidence>,
    last_extension_poll: Option<Instant>,
    last_endpoint_polls: HashMap<String, Instant>,
}

struct PendingBrowserActuation {
    dispatch_id: Option<String>,
    expected_generation: i64,
    timed_out_at: Option<Instant>,
}

impl Default for BrowserActuationBroker {
    fn default() -> Self {
        let (wake_tx, _) = tokio::sync::watch::channel(0_u64);
        Self {
            inner: Arc::new((Mutex::new(BrowserActuationState::default()), Condvar::new())),
            wake_tx,
            timeout: BROWSER_ACTUATION_TIMEOUT,
            create_timeout: BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT,
            late_completion_ttl: BROWSER_LATE_COMPLETION_TTL,
        }
    }
}

impl BrowserActuationBroker {
    #[cfg(test)]
    pub(super) fn with_durations(timeout: Duration, late_completion_ttl: Duration) -> Self {
        let (wake_tx, _) = tokio::sync::watch::channel(0_u64);
        Self {
            inner: Arc::new((Mutex::new(BrowserActuationState::default()), Condvar::new())),
            wake_tx,
            timeout,
            create_timeout: timeout,
            late_completion_ttl,
        }
    }

    pub(super) fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
        self.wake_tx.subscribe()
    }

    fn prune_expired(&self, state: &mut BrowserActuationState) {
        let expired = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .timed_out_at
                    .is_some_and(|timed_out_at| timed_out_at.elapsed() > self.late_completion_ttl)
            })
            .map(|(actuation_id, _)| actuation_id.clone())
            .collect::<Vec<_>>();
        for actuation_id in expired {
            state.pending.remove(&actuation_id);
            state.completions.remove(&actuation_id);
            state.queued.retain(|command| {
                command.get("actuation_id").and_then(Value::as_str) != Some(actuation_id.as_str())
            });
        }
    }

    /// Milliseconds since this endpoint's last `/push/events` poll, when it was
    /// ever polled. `None` means the endpoint was never seen in this runtime
    /// generation, which is not the same as an endpoint that went quiet.
    fn last_seen_age_ms(state: &BrowserActuationState, endpoint_ref: Option<&str>) -> Option<u64> {
        let endpoint_ref = endpoint_ref?;
        let seen = state.last_endpoint_polls.get(endpoint_ref)?;
        u64::try_from(seen.elapsed().as_millis()).ok()
    }

    fn note_poll(state: &mut BrowserActuationState, endpoint_ref: Option<&str>) {
        let now = Instant::now();
        state.last_extension_poll = Some(now);
        if let Some(endpoint_ref) = endpoint_ref.filter(|value| !value.is_empty()) {
            state
                .last_endpoint_polls
                .insert(endpoint_ref.to_owned(), now);
        }
        state
            .last_endpoint_polls
            .retain(|_, seen| seen.elapsed() <= BROWSER_ENDPOINT_LAST_SEEN_RETENTION);
    }

    pub(super) fn take_next_for_extension(&self, endpoint_ref: Option<&str>) -> Option<Value> {
        let Ok(mut state) = self.inner.0.lock() else {
            return None;
        };
        self.prune_expired(&mut state);
        Self::note_poll(&mut state, endpoint_ref);
        let position = state.queued.iter().position(|command| {
            command
                .get("target_endpoint_ref")
                .and_then(Value::as_str)
                .is_none_or(|target| endpoint_ref == Some(target))
        })?;
        state.queued.remove(position)
    }

    pub(super) fn note_extension_poll(&self, endpoint_ref: Option<&str>) {
        let Ok(mut state) = self.inner.0.lock() else {
            return;
        };
        self.prune_expired(&mut state);
        Self::note_poll(&mut state, endpoint_ref);
    }

    pub(super) fn complete(
        &self,
        actuation_id: &str,
        evidence: BrowserPostconditionEvidence,
    ) -> Result<(), String> {
        let (lock, ready) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
        self.prune_expired(&mut state);
        if !state.pending.contains_key(actuation_id) {
            return Err("browser_actuation_not_pending".to_owned());
        }
        state.completions.insert(actuation_id.to_owned(), evidence);
        ready.notify_all();
        Ok(())
    }

    fn extension_live(state: &BrowserActuationState, endpoint_ref: Option<&str>) -> bool {
        if let Some(endpoint_ref) = endpoint_ref {
            return state
                .last_endpoint_polls
                .get(endpoint_ref)
                .is_some_and(|seen| seen.elapsed() <= BROWSER_EXTENSION_LIVE_WINDOW);
        }
        state
            .last_extension_poll
            .is_some_and(|seen| seen.elapsed() <= BROWSER_EXTENSION_LIVE_WINDOW)
    }
}

#[cfg(test)]
impl BrowserActuationBroker {
    /// Test-only liveness probe for one browser endpoint. The HTTP push/SSE route
    /// test asserts that the stream refreshed extension liveness without
    /// exposing the broker's lock or state to the parent module.
    pub(super) fn extension_live_for_endpoint(&self, endpoint_ref: &str) -> bool {
        self.endpoint_live(endpoint_ref).unwrap_or(false)
    }
}

impl BrowserActuator for BrowserActuationBroker {
    /// Liveness from the live extension SSE heartbeat window, not from the
    /// endpoint's historical `last_observed_at`. `None` (unknown) is reserved
    /// for a broken broker lock; an endpoint without a fresh heartbeat is
    /// `Some(false)` (stale).
    fn endpoint_live(&self, endpoint_ref: &str) -> Option<bool> {
        let state = self.inner.0.lock().ok()?;
        Some(Self::extension_live(&state, Some(endpoint_ref)))
    }

    fn actuate(
        &self,
        operation: &str,
        params: &Value,
        expected_generation: i64,
        dispatch_id: Option<&str>,
    ) -> Result<BrowserPostconditionEvidence, String> {
        let target_endpoint_ref = params
            .get(BROWSER_ACTUATION_ENDPOINT_PARAM)
            .and_then(Value::as_str);
        let mut command_params = params.clone();
        if let Some(object) = command_params.as_object_mut() {
            object.remove(BROWSER_ACTUATION_ENDPOINT_PARAM);
        }
        let actuation_id = format!(
            "ba_{:016x}",
            NEXT_BROWSER_ACTUATION.fetch_add(1, Ordering::Relaxed)
        );
        let (lock, ready) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
        self.prune_expired(&mut state);
        if !Self::extension_live(&state, target_endpoint_ref) {
            // #629: no `/push/events` poll inside the liveness window, so this
            // command provably never left the runtime. The endpoint's last-seen
            // age separates "went quiet 45s ago" from "never seen"
            // (`last_seen_ms=never`), which is exactly what the 2026-10-13
            // `browser_page_unavailable` repro could not tell apart.
            let endpoint_last_seen_age_ms = Self::last_seen_age_ms(&state, target_endpoint_ref);
            emit_browser_endpoint_event(
                "stale",
                &[
                    ("endpoint_ref", log_endpoint_ref(target_endpoint_ref)),
                    ("operation", operation.to_owned()),
                    ("actuation_id", actuation_id.clone()),
                    ("last_seen_ms", log_last_seen_ms(endpoint_last_seen_age_ms)),
                    (
                        "window_ms",
                        BROWSER_EXTENSION_LIVE_WINDOW.as_millis().to_string(),
                    ),
                ],
            );
            return Ok(BrowserPostconditionEvidence {
                observed_generation: expected_generation,
                command_accepted: false,
                browser_online: false,
                resource_available: true,
                rejected: false,
                stable_resource_ref_observed: false,
                lifecycle_observed: false,
                canonical_url_observed: false,
                accepted_message_observed: false,
                message_baseline_advanced: false,
                reasoning_effort_readback: None,
                required_apps_readback: Vec::new(),
                generation_owner: None,
                generation_status_observed: false,
                generation_stopped: false,
                endpoint_last_seen_age_ms,
                result: None,
            });
        }
        state.pending.insert(
            actuation_id.clone(),
            PendingBrowserActuation {
                dispatch_id: dispatch_id.map(str::to_owned),
                expected_generation,
                timed_out_at: None,
            },
        );
        state.queued.push_back(json!({
            "protocol": "herdr-browser-actuation/v1",
            "actuation_id": actuation_id,
            "dispatch_id": dispatch_id,
            "operation": operation,
            "expected_generation": expected_generation,
            "target_endpoint_ref": target_endpoint_ref,
            "params": command_params,
        }));
        ready.notify_all();
        self.wake_tx.send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });

        let timeout = if operation == "herdr_mcp.browser_session.create" {
            self.create_timeout
        } else if operation == "herdr_mcp.browser_dispatch.reconcile" {
            self.timeout.min(Duration::from_secs(6))
        } else {
            self.timeout
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(evidence) = state.completions.remove(&actuation_id) {
                state.pending.remove(&actuation_id);
                return Ok(evidence);
            }
            let now = Instant::now();
            if now >= deadline {
                // A command still present in `queued` was never taken by an
                // extension poll, because `take_next_for_extension` uses this
                // same mutex: this check-and-remove is atomic and a concurrent
                // take cannot interleave. mcp.rs maps command_accepted=false to
                // a retryable not_delivered response (e.g.
                // browser_page_unavailable delivery_state not_delivered), safe to
                // retry because the command provably never crossed the SSE
                // boundary.
                let queued_position = state.queued.iter().position(|command| {
                    command.get("actuation_id").and_then(Value::as_str)
                        == Some(actuation_id.as_str())
                });
                if let Some(position) = queued_position {
                    state.queued.remove(position);
                    state.pending.remove(&actuation_id);
                    emit_browser_endpoint_event(
                        "command_timeout",
                        &[
                            ("endpoint_ref", log_endpoint_ref(target_endpoint_ref)),
                            ("operation", operation.to_owned()),
                            ("actuation_id", actuation_id.clone()),
                            ("dispatch_id", dispatch_id.unwrap_or("-").to_owned()),
                            ("timeout_ms", timeout.as_millis().to_string()),
                            ("delivery_state", "not_delivered".to_owned()),
                            ("reason", "queued_untaken_at_timeout".to_owned()),
                            (
                                "last_seen_ms",
                                log_last_seen_ms(Self::last_seen_age_ms(
                                    &state,
                                    target_endpoint_ref,
                                )),
                            ),
                        ],
                    );
                    return Ok(BrowserPostconditionEvidence {
                        observed_generation: expected_generation,
                        command_accepted: false,
                        browser_online: true,
                        resource_available: true,
                        rejected: false,
                        stable_resource_ref_observed: false,
                        lifecycle_observed: false,
                        canonical_url_observed: false,
                        accepted_message_observed: false,
                        message_baseline_advanced: false,
                        reasoning_effort_readback: None,
                        required_apps_readback: Vec::new(),
                        generation_owner: None,
                        generation_status_observed: false,
                        generation_stopped: false,
                        endpoint_last_seen_age_ms: None,
                        result: None,
                    });
                }
                if dispatch_id.is_some() {
                    if let Some(pending) = state.pending.get_mut(&actuation_id) {
                        pending.timed_out_at = Some(now);
                    }
                } else {
                    state.pending.remove(&actuation_id);
                }
                state.queued.retain(|command| {
                    command.get("actuation_id").and_then(Value::as_str)
                        != Some(actuation_id.as_str())
                });
                emit_browser_endpoint_event(
                    "command_timeout",
                    &[
                        ("endpoint_ref", log_endpoint_ref(target_endpoint_ref)),
                        ("operation", operation.to_owned()),
                        ("actuation_id", actuation_id.clone()),
                        ("dispatch_id", dispatch_id.unwrap_or("-").to_owned()),
                        ("timeout_ms", timeout.as_millis().to_string()),
                        ("delivery_state", "delivery_unknown".to_owned()),
                        ("reason", "completion_timeout".to_owned()),
                        (
                            "last_seen_ms",
                            log_last_seen_ms(Self::last_seen_age_ms(&state, target_endpoint_ref)),
                        ),
                    ],
                );
                // Once a command may have crossed the SSE boundary, a missing
                // acknowledgement is delivery-uncertain and must never invite replay.
                return Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: false,
                    lifecycle_observed: false,
                    canonical_url_observed: false,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: None,
                    generation_status_observed: false,
                    generation_stopped: false,
                    endpoint_last_seen_age_ms: None,
                    result: None,
                });
            }
            let timeout = deadline.saturating_duration_since(now);
            let waited = ready
                .wait_timeout(state, timeout)
                .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
            state = waited.0;
        }
    }

    fn actuate_for_endpoint(
        &self,
        operation: &str,
        params: &Value,
        expected_generation: i64,
        endpoint_ref: Option<&str>,
        dispatch_id: Option<&str>,
    ) -> Result<BrowserPostconditionEvidence, String> {
        let mut routed_params = params.clone();
        if let Some(endpoint_ref) = endpoint_ref {
            crate::state_store::validate_browser_endpoint_ref(endpoint_ref)?;
            let Some(object) = routed_params.as_object_mut() else {
                return Err("browser_actuation_params_invalid".to_owned());
            };
            object.insert(
                BROWSER_ACTUATION_ENDPOINT_PARAM.to_owned(),
                json!(endpoint_ref),
            );
        }
        self.actuate(operation, &routed_params, expected_generation, dispatch_id)
    }

    fn reconcile_dispatch(
        &self,
        dispatch_id: &str,
        expected_generation: i64,
    ) -> Result<Option<BrowserPostconditionEvidence>, String> {
        let (lock, _) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
        self.prune_expired(&mut state);
        let actuation_id = state.pending.iter().find_map(|(actuation_id, pending)| {
            (pending.dispatch_id.as_deref() == Some(dispatch_id)
                && pending.expected_generation == expected_generation)
                .then(|| actuation_id.clone())
        });
        let Some(actuation_id) = actuation_id else {
            return Ok(None);
        };
        let Some(evidence) = state.completions.remove(&actuation_id) else {
            return Ok(None);
        };
        state.pending.remove(&actuation_id);
        if !evidence.resource_available
            && evidence
                .result
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(Value::as_str)
                == Some("browser_session_content_dispatch_failed")
        {
            return Ok(None);
        }
        Ok(Some(evidence))
    }
}

// The broker is exercised directly here: the command queue, liveness window,
// dispatch reconciliation, and timeout settlement are broker behavior, while
// the trusted-IPC route test stays with the HTTP surface in the parent module.
#[cfg(test)]
mod tests {
    use super::super::SSE_HEARTBEAT;
    use super::*;

    #[test]
    fn endpoint_live_reports_per_endpoint_heartbeat_liveness() {
        let broker = BrowserActuationBroker::default();
        let endpoint_a = format!("bep_{}", "a".repeat(64));
        let endpoint_b = format!("bep_{}", "b".repeat(64));

        assert_eq!(
            broker.endpoint_live(&endpoint_a),
            Some(false),
            "an endpoint that never sent an extension heartbeat is stale, not unknown"
        );

        broker.note_extension_poll(Some(&endpoint_a));
        assert_eq!(broker.endpoint_live(&endpoint_a), Some(true));
        assert_eq!(
            broker.endpoint_live(&endpoint_b),
            Some(false),
            "liveness is per endpoint and must not leak across endpoints"
        );

        // Force the heartbeat past the live window without sleeping.
        {
            let (lock, _) = &*broker.inner;
            let mut state = lock.lock().unwrap();
            state.last_endpoint_polls.insert(
                endpoint_a.clone(),
                Instant::now() - BROWSER_EXTENSION_LIVE_WINDOW - Duration::from_secs(1),
            );
        }
        assert_eq!(
            broker.endpoint_live(&endpoint_a),
            Some(false),
            "an endpoint whose heartbeat aged past the live window must be stale"
        );
    }

    #[test]
    fn browser_actuation_liveness_refreshes_on_idle_sse_heartbeat() {
        let broker = BrowserActuationBroker::default();
        {
            let (lock, _) = &*broker.inner;
            let mut state = lock.lock().unwrap();
            state.last_extension_poll =
                Some(Instant::now() - BROWSER_EXTENSION_LIVE_WINDOW - Duration::from_secs(1));
            assert!(!BrowserActuationBroker::extension_live(&state, None));
        }

        broker.note_extension_poll(None);

        let (lock, _) = &*broker.inner;
        let state = lock.lock().unwrap();
        assert!(BrowserActuationBroker::extension_live(&state, None));
    }

    fn issue_632_actuate_in_background(
        broker: &BrowserActuationBroker,
        endpoint_ref: &str,
        dispatch_id: Option<&'static str>,
    ) -> tokio::task::JoinHandle<Result<BrowserPostconditionEvidence, String>> {
        let task_broker = broker.clone();
        let endpoint_ref = endpoint_ref.to_owned();
        tokio::task::spawn_blocking(move || {
            task_broker.actuate_for_endpoint(
                "herdr_mcp.browser_page.action",
                &json!({"page_ref":"bp_test","action":"open"}),
                3,
                Some(&endpoint_ref),
                dispatch_id,
            )
        })
    }

    /// #632: a command that no extension poll ever took is provably not delivered.
    #[tokio::test]
    async fn queued_untaken_timeout_is_not_delivered_and_retryable() {
        for dispatch_id in [None, Some("op:issue632:untaken")] {
            let broker = BrowserActuationBroker::with_durations(
                Duration::from_millis(150),
                Duration::from_millis(150),
            );
            let endpoint_ref = format!("bep_{}", "c".repeat(64));
            broker.note_extension_poll(Some(&endpoint_ref));
            let evidence = issue_632_actuate_in_background(&broker, &endpoint_ref, dispatch_id)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !evidence.command_accepted,
                "queued-untaken timeout must be not_delivered (command_accepted=false), dispatch={dispatch_id:?}"
            );
            assert!(evidence.browser_online);
            assert!(evidence.result.is_none());
            // #629: a timeout that never crossed the SSE boundary carries no
            // last-seen age, because the endpoint was live when it was queued.
            assert_eq!(evidence.endpoint_last_seen_age_ms, None);
        }
    }

    /// #629: an endpoint with no poll inside the liveness window is refused
    /// before the command is ever queued. That is not a timeout, so it must
    /// report `browser_online: false` and no last-seen age when the endpoint was
    /// never seen in this runtime generation.
    #[tokio::test]
    async fn offline_endpoint_refusal_never_waits_for_the_actuation_deadline() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_secs(30),
            Duration::from_secs(30),
        );
        let endpoint_ref = format!("bep_{}", "1".repeat(64));
        let started = Instant::now();
        let evidence = issue_632_actuate_in_background(&broker, &endpoint_ref, None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an offline endpoint must be refused without waiting for the actuation deadline"
        );
        assert!(!evidence.browser_online);
        assert!(!evidence.command_accepted);
        assert!(evidence.result.is_none());
        assert_eq!(evidence.endpoint_last_seen_age_ms, None);
    }

    /// #629: an endpoint that went quiet reports how long ago it was last seen,
    /// so a closed browser is distinguishable from a browser that never polled.
    #[tokio::test]
    async fn stale_endpoint_refusal_reports_last_seen_age() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_secs(30),
            Duration::from_secs(30),
        );
        let endpoint_ref = format!("bep_{}", "2".repeat(64));
        broker.note_extension_poll(Some(&endpoint_ref));
        {
            let (lock, _) = &*broker.inner;
            let mut state = lock.lock().unwrap();
            state.last_endpoint_polls.insert(
                endpoint_ref.clone(),
                Instant::now() - Duration::from_secs(45),
            );
        }
        let evidence = issue_632_actuate_in_background(&broker, &endpoint_ref, None)
            .await
            .unwrap()
            .unwrap();
        assert!(!evidence.browser_online);
        assert!(!evidence.command_accepted);
        assert!(
            evidence
                .endpoint_last_seen_age_ms
                .is_some_and(|age| age >= 45_000),
            "a quiet endpoint must report its last-seen age: {:?}",
            evidence.endpoint_last_seen_age_ms
        );
    }

    /// #632: after a queued-untaken timeout the command is gone from the queue,
    /// so it can never be delivered late (which would make the retry a duplicate).
    #[tokio::test]
    async fn timed_out_untaken_command_is_never_delivered_late() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(150),
            Duration::from_millis(150),
        );
        let endpoint_ref = format!("bep_{}", "d".repeat(64));
        broker.note_extension_poll(Some(&endpoint_ref));
        let evidence =
            issue_632_actuate_in_background(&broker, &endpoint_ref, Some("op:issue632:late"))
                .await
                .unwrap()
                .unwrap();
        assert!(!evidence.command_accepted);
        assert!(
            broker
                .take_next_for_extension(Some(&endpoint_ref))
                .is_none()
        );
        assert!(broker.take_next_for_extension(None).is_none());
        let (lock, _) = &*broker.inner;
        let state = lock.lock().unwrap();
        assert!(state.queued.is_empty());
        assert!(
            state.pending.is_empty(),
            "a never-delivered command leaves no pending entry for a late completion"
        );
    }

    /// #632: once an extension poll took the command, a missing acknowledgement
    /// stays delivery-uncertain (command_accepted=true) and must not invite replay.
    #[tokio::test]
    async fn handed_out_command_timeout_stays_delivery_unknown() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(300),
            Duration::from_millis(300),
        );
        let endpoint_ref = format!("bep_{}", "e".repeat(64));
        broker.note_extension_poll(Some(&endpoint_ref));
        let mut wake_rx = broker.subscribe_wake();
        let task = issue_632_actuate_in_background(&broker, &endpoint_ref, None);
        tokio::time::timeout(Duration::from_millis(200), wake_rx.changed())
            .await
            .expect("enqueue wakes push")
            .expect("wake channel open");
        let command = broker
            .take_next_for_extension(Some(&endpoint_ref))
            .expect("extension takes the command");
        assert_eq!(command["target_endpoint_ref"], endpoint_ref);
        let evidence = task.await.unwrap().unwrap();
        assert!(
            evidence.command_accepted,
            "handed-out command keeps delivery_unknown"
        );
        assert!(evidence.browser_online);
        assert_eq!(
            evidence.endpoint_last_seen_age_ms, None,
            "a post-handoff timeout is classified by command_accepted, not by liveness age"
        );
    }

    /// #629: every endpoint lifecycle line is structured and content-free: ids,
    /// enum codes, timestamps and millisecond durations only. No URL, query
    /// string, title, page content or profile seed may reach the log.
    #[tokio::test]
    async fn endpoint_lifecycle_log_lines_carry_ids_and_no_page_content() {
        fn assert_content_free(line: &str) {
            assert!(
                line.starts_with("herdr-mcp: browser endpoint event="),
                "{line}"
            );
            assert_eq!(line.lines().count(), 1, "{line}");
            for needle in [
                "http",
                "://",
                "?",
                "page_ref",
                "bp_",
                "title",
                "profile_seed",
            ] {
                assert!(
                    !line.contains(needle),
                    "lifecycle log line must stay content-free ({needle}): {line}"
                );
            }
            for token in line.split(' ').skip(4) {
                assert!(
                    token.contains('='),
                    "every lifecycle log field must be key=value: {line}"
                );
            }
        }

        // offline endpoint: the command is refused before it is queued.
        let offline_broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(150),
            Duration::from_millis(150),
        );
        let offline_endpoint = format!("bep_{}", "4".repeat(64));
        let _ = issue_632_actuate_in_background(&offline_broker, &offline_endpoint, None)
            .await
            .unwrap()
            .unwrap();
        let offline_lines = endpoint_log_capture::take_for_endpoint(&offline_endpoint);
        assert!(
            offline_lines
                .iter()
                .any(|line| line.contains("event=stale") && line.contains("last_seen_ms=")),
            "an offline refusal must log the endpoint's last-seen age: {offline_lines:?}"
        );

        // queued but never taken: provably not delivered at the deadline.
        let untaken_broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(150),
            Duration::from_millis(150),
        );
        let untaken_endpoint = format!("bep_{}", "5".repeat(64));
        untaken_broker.note_extension_poll(Some(&untaken_endpoint));
        let _ = issue_632_actuate_in_background(&untaken_broker, &untaken_endpoint, None)
            .await
            .unwrap()
            .unwrap();
        let untaken_lines = endpoint_log_capture::take_for_endpoint(&untaken_endpoint);
        assert!(
            untaken_lines
                .iter()
                .any(|line| line.contains("event=command_timeout")
                    && line.contains("reason=queued_untaken_at_timeout")
                    && line.contains("delivery_state=not_delivered")),
            "a queued-untaken timeout must be logged as not delivered: {untaken_lines:?}"
        );

        // handed to the extension's SSE stream without a completion.
        let handed_out_broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(300),
            Duration::from_millis(300),
        );
        let handed_out_endpoint = format!("bep_{}", "6".repeat(64));
        handed_out_broker.note_extension_poll(Some(&handed_out_endpoint));
        let mut wake_rx = handed_out_broker.subscribe_wake();
        let task = issue_632_actuate_in_background(&handed_out_broker, &handed_out_endpoint, None);
        tokio::time::timeout(Duration::from_millis(200), wake_rx.changed())
            .await
            .expect("enqueue wakes push")
            .expect("wake channel open");
        assert!(
            handed_out_broker
                .take_next_for_extension(Some(&handed_out_endpoint))
                .is_some()
        );
        let _ = task.await.unwrap().unwrap();
        let handed_out_lines = endpoint_log_capture::take_for_endpoint(&handed_out_endpoint);
        assert!(
            handed_out_lines
                .iter()
                .any(|line| line.contains("event=command_timeout")
                    && line.contains("reason=completion_timeout")
                    && line.contains("delivery_state=delivery_unknown")),
            "a post-handoff timeout must be logged as delivery unknown: {handed_out_lines:?}"
        );

        for line in offline_lines
            .iter()
            .chain(untaken_lines.iter())
            .chain(handed_out_lines.iter())
        {
            assert_content_free(line);
        }
    }

    #[tokio::test]
    async fn browser_actuation_is_consumed_only_by_the_target_endpoint() {
        let broker =
            BrowserActuationBroker::with_durations(Duration::from_secs(1), Duration::from_secs(1));
        let endpoint_a = format!("bep_{}", "a".repeat(64));
        let endpoint_b = format!("bep_{}", "b".repeat(64));
        broker.note_extension_poll(Some(&endpoint_a));
        broker.note_extension_poll(Some(&endpoint_b));
        let mut wake_rx = broker.subscribe_wake();

        let task_broker = broker.clone();
        let task_endpoint = endpoint_a.clone();
        let task = tokio::task::spawn_blocking(move || {
            task_broker.actuate_for_endpoint(
                "herdr_mcp.browser_dispatch.submit",
                &json!({"session_ref":"br_test","message":"hello"}),
                7,
                Some(&task_endpoint),
                None,
            )
        });
        tokio::time::timeout(Duration::from_millis(100), wake_rx.changed())
            .await
            .expect("actuation enqueue must wake push far before the 15s heartbeat")
            .expect("browser actuation wake channel stays open");

        assert!(
            broker.take_next_for_extension(Some(&endpoint_b)).is_none(),
            "a different browser endpoint must not consume the command"
        );
        let command = broker
            .take_next_for_extension(Some(&endpoint_a))
            .expect("target endpoint must receive its command");
        assert_eq!(command["target_endpoint_ref"], endpoint_a);
        let actuation_id = command["actuation_id"].as_str().unwrap().to_owned();
        broker
            .complete(
                &actuation_id,
                BrowserPostconditionEvidence {
                    observed_generation: 7,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: true,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(7),
                    generation_status_observed: true,
                    generation_stopped: false,
                    endpoint_last_seen_age_ms: None,
                    result: None,
                },
            )
            .unwrap();
        assert!(task.await.unwrap().unwrap().accepted_message_observed);
    }

    #[test]
    fn browser_actuation_timeouts_cover_ordinary_edge_and_create_budgets() {
        assert!(BROWSER_EXTENSION_LIVE_WINDOW > SSE_HEARTBEAT);
        assert!(BROWSER_ACTUATION_TIMEOUT > SSE_HEARTBEAT);
        assert!(BROWSER_ACTUATION_TIMEOUT <= Duration::from_secs(22));
        assert!(BROWSER_ACTUATION_TIMEOUT < Duration::from_secs(30));
        assert!(BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT > BROWSER_ACTUATION_TIMEOUT);
        // The create wait bounds the extension-side create worst case: the 30s
        // create-scope content-script handshake plus the 43s content acceptance
        // leg (see BROWSER_CREATE_SCOPE_HANDSHAKE_TIMEOUT_MS and
        // BROWSER_CREATE_CONTENT_TIMEOUT_MS in extension/background.js).
        assert!(BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT > Duration::from_secs(30 + 43));
        assert!(BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT <= Duration::from_secs(80));
    }

    #[tokio::test]
    async fn browser_actuation_timeout_retains_only_exact_dispatch_late_completion() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(20),
            Duration::from_millis(250),
        );
        assert!(broker.take_next_for_extension(None).is_none());
        let dispatch_id = format!("bd_{}", "a".repeat(64));
        let expected_dispatch_id = dispatch_id.clone();
        let broker_for_task = broker.clone();
        let task = tokio::task::spawn_blocking(move || {
            broker_for_task.actuate(
                "herdr_mcp.browser_dispatch.submit",
                &json!({"session_ref":"br_test","message":"hello"}),
                7,
                Some(&expected_dispatch_id),
            )
        });
        let command = loop {
            if let Some(command) = broker.take_next_for_extension(None) {
                break command;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        let actuation_id = command["actuation_id"].as_str().unwrap().to_owned();
        let timed_out = task.await.unwrap().unwrap();
        assert!(timed_out.command_accepted);
        assert!(!timed_out.accepted_message_observed);

        let late = BrowserPostconditionEvidence {
            observed_generation: 7,
            command_accepted: true,
            browser_online: true,
            resource_available: true,
            rejected: false,
            stable_resource_ref_observed: true,
            lifecycle_observed: true,
            canonical_url_observed: true,
            accepted_message_observed: true,
            message_baseline_advanced: false,
            reasoning_effort_readback: None,
            required_apps_readback: Vec::new(),
            generation_owner: Some(7),
            generation_status_observed: true,
            generation_stopped: false,
            endpoint_last_seen_age_ms: None,
            result: None,
        };
        broker.complete(&actuation_id, late.clone()).unwrap();
        assert!(
            broker
                .reconcile_dispatch(&dispatch_id, 8)
                .unwrap()
                .is_none(),
            "a newer generation must not consume old-generation evidence"
        );
        assert_eq!(
            broker.reconcile_dispatch(&dispatch_id, 7).unwrap(),
            Some(late)
        );
        assert!(
            broker
                .reconcile_dispatch(&dispatch_id, 7)
                .unwrap()
                .is_none(),
            "late completion is one-use settlement evidence"
        );
    }

    #[tokio::test]
    async fn browser_actuation_late_completion_expires_boundedly() {
        let broker = BrowserActuationBroker::with_durations(
            Duration::from_millis(10),
            Duration::from_millis(10),
        );
        assert!(broker.take_next_for_extension(None).is_none());
        let dispatch_id = format!("bd_{}", "b".repeat(64));
        let expected_dispatch_id = dispatch_id.clone();
        let broker_for_task = broker.clone();
        let task = tokio::task::spawn_blocking(move || {
            broker_for_task.actuate(
                "herdr_mcp.browser_dispatch.submit",
                &json!({"session_ref":"br_test","message":"hello"}),
                7,
                Some(&expected_dispatch_id),
            )
        });
        let command = loop {
            if let Some(command) = broker.take_next_for_extension(None) {
                break command;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        let actuation_id = command["actuation_id"].as_str().unwrap().to_owned();
        let _ = task.await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            broker.complete(
                &actuation_id,
                BrowserPostconditionEvidence {
                    observed_generation: 7,
                    command_accepted: false,
                    browser_online: true,
                    resource_available: false,
                    rejected: false,
                    stable_resource_ref_observed: false,
                    lifecycle_observed: false,
                    canonical_url_observed: false,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: None,
                    generation_status_observed: false,
                    generation_stopped: false,
                    endpoint_last_seen_age_ms: None,
                    result: None,
                }
            ),
            Err("browser_actuation_not_pending".to_owned())
        );
        assert!(
            broker
                .reconcile_dispatch(&dispatch_id, 7)
                .unwrap()
                .is_none()
        );
    }
}
