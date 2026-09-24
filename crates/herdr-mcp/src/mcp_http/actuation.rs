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

// Edge's default tool-request deadline is 30s. Keep one browser actuation wait
// above the 15s extension heartbeat while leaving room for bounded create
// reconciliation plus HTTP/serialization overhead before the outer request
// can turn a durable reservation/dispatch into an ambiguous gateway timeout.
const BROWSER_ACTUATION_TIMEOUT: Duration = Duration::from_secs(22);
const BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT: Duration = Duration::from_secs(53);
const BROWSER_LATE_COMPLETION_TTL: Duration = Duration::from_secs(60);
const BROWSER_ACTUATION_ENDPOINT_PARAM: &str = "__herdr_browser_endpoint_ref";
// The extension polls for browser actuation on the shared SSE heartbeat. Keep
// the liveness window above that 15s cadence so an idle healthy stream is not
// classified offline between polls.
const BROWSER_EXTENSION_LIVE_WINDOW: Duration = Duration::from_secs(20);

static NEXT_BROWSER_ACTUATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(super) struct BrowserActuationBroker {
    inner: Arc<(Mutex<BrowserActuationState>, Condvar)>,
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
        Self {
            inner: Arc::new((Mutex::new(BrowserActuationState::default()), Condvar::new())),
            timeout: BROWSER_ACTUATION_TIMEOUT,
            create_timeout: BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT,
            late_completion_ttl: BROWSER_LATE_COMPLETION_TTL,
        }
    }
}

impl BrowserActuationBroker {
    #[cfg(test)]
    fn with_durations(timeout: Duration, late_completion_ttl: Duration) -> Self {
        Self {
            inner: Arc::new((Mutex::new(BrowserActuationState::default()), Condvar::new())),
            timeout,
            create_timeout: timeout,
            late_completion_ttl,
        }
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

    fn note_poll(state: &mut BrowserActuationState, endpoint_ref: Option<&str>) {
        let now = Instant::now();
        state.last_extension_poll = Some(now);
        if let Some(endpoint_ref) = endpoint_ref.filter(|value| !value.is_empty()) {
            state
                .last_endpoint_polls
                .insert(endpoint_ref.to_owned(), now);
        }
        state.last_endpoint_polls.retain(|_, seen| {
            seen.elapsed() <= BROWSER_EXTENSION_LIVE_WINDOW + BROWSER_EXTENSION_LIVE_WINDOW
        });
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
        let Ok(state) = self.inner.0.lock() else {
            return false;
        };
        Self::extension_live(&state, Some(endpoint_ref))
    }
}

impl BrowserActuator for BrowserActuationBroker {
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

        let timeout = if operation == "herdr_mcp.browser_session.create" {
            self.create_timeout
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

    #[tokio::test]
    async fn browser_actuation_is_consumed_only_by_the_target_endpoint() {
        let broker =
            BrowserActuationBroker::with_durations(Duration::from_secs(1), Duration::from_secs(1));
        let endpoint_a = format!("bep_{}", "a".repeat(64));
        let endpoint_b = format!("bep_{}", "b".repeat(64));
        broker.note_extension_poll(Some(&endpoint_a));
        broker.note_extension_poll(Some(&endpoint_b));

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
        loop {
            if !broker.inner.0.lock().unwrap().queued.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

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
                    result: None,
                },
            )
            .unwrap();
        assert!(task.await.unwrap().unwrap().accepted_message_observed);
    }

    #[test]
    fn browser_actuation_timeout_leaves_edge_request_headroom() {
        assert!(BROWSER_EXTENSION_LIVE_WINDOW > SSE_HEARTBEAT);
        assert!(BROWSER_ACTUATION_TIMEOUT > SSE_HEARTBEAT);
        assert!(BROWSER_ACTUATION_TIMEOUT <= Duration::from_secs(22));
        assert!(BROWSER_ACTUATION_TIMEOUT < Duration::from_secs(30));
        assert!(BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT > BROWSER_ACTUATION_TIMEOUT);
        assert!(BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT < Duration::from_secs(60));
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
