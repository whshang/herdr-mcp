//! Async runtime-dispatch coordinator for the staged Rust Link transport.
//!
//! This is the composition layer between [`LinkTransportCore`],
//! [`PendingRequests`] and an injected [`LinkRuntimeTransport`]. It owns
//! first-settler-wins request races while generation ownership stays inside the
//! runtime transport. Generation-aware transports reserve exactly one request
//! lease at accept time and dispatch through that same lease.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Number, Value, json};
use tokio::sync::mpsc;

use super::local_mcp::{LinkRuntimeTransport, RuntimeHealth, RuntimeToolResult};
use super::request_core::{
    CODE_DUPLICATE_REQUEST, CODE_LINK_STOPPING, CODE_QUEUE_FULL, CODE_REQUEST_TIMEOUT,
    LINK_DEFAULT_MAX_PENDING, LINK_DEFAULT_REQUEST_TIMEOUT_MS, PendingInsertError, PendingRequests,
    PendingSlot, RuntimeRequest, clamp_request_timeout,
};
use super::transport::{LinkTransportCore, TransportAction, TransportError};
use crate::relay::protocol::{DeliveryState, RelayMessage, RuntimeContractInfo};
use crate::relay::wire::{
    build_cancel_ack_message, build_heartbeat_message, build_hello_message, build_status_report,
    build_tool_error_message, build_tool_result_message,
};

const CODE_RUNTIME_GENERATION_DISPATCH_REJECTED: &str = "runtime_generation_dispatch_rejected";

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub workstation_id: String,
    pub boot_id: String,
    pub started_at_ms: i64,
    pub request_timeout_ms: u64,
    pub max_pending: usize,
}

impl RunnerConfig {
    pub fn new(
        workstation_id: impl Into<String>,
        boot_id: impl Into<String>,
        started_at_ms: i64,
    ) -> Self {
        Self {
            workstation_id: workstation_id.into(),
            boot_id: boot_id.into(),
            started_at_ms,
            request_timeout_ms: LINK_DEFAULT_REQUEST_TIMEOUT_MS,
            max_pending: LINK_DEFAULT_MAX_PENDING,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunnerError {
    Transport(TransportError),
    RuntimeEventChannelClosed,
}

impl From<TransportError> for RunnerError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

#[derive(Debug)]
enum RuntimeEvent {
    DispatchCompleted {
        slot: PendingSlot,
        serving_generation: String,
        result: RuntimeToolResult,
    },
    RequestTimeout {
        slot: PendingSlot,
    },
    StatusCompleted {
        health: RuntimeHealth,
    },
}

pub struct LinkRunnerCore<T: LinkRuntimeTransport> {
    config: RunnerConfig,
    runtime: Arc<T>,
    pending: PendingRequests,
    event_tx: mpsc::Sender<RuntimeEvent>,
    event_rx: mpsc::Receiver<RuntimeEvent>,
    stopping: bool,
}

impl<T: LinkRuntimeTransport> LinkRunnerCore<T> {
    pub fn new(config: RunnerConfig, runtime: Arc<T>) -> Self {
        let max_pending = config.max_pending.max(1);
        let event_capacity = max_pending.saturating_mul(2).max(8);
        let (event_tx, event_rx) = mpsc::channel(event_capacity);
        Self {
            config,
            runtime,
            pending: PendingRequests::new(max_pending),
            event_tx,
            event_rx,
            stopping: false,
        }
    }

    pub fn active_requests(&self) -> usize {
        self.pending.len()
    }

    pub fn stopping(&self) -> bool {
        self.stopping
    }

    pub fn runtime_info(&self) -> RuntimeContractInfo {
        self.runtime.runtime_info()
    }

    pub fn hello_message(&self, connected_at_ms: i64) -> RelayMessage {
        RelayMessage::Hello(build_hello_message(
            self.config.workstation_id.clone(),
            self.config.boot_id.clone(),
            super::request_core::LINK_VERSION,
            super::request_core::DEFAULT_CAPABILITIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            self.runtime.runtime_info(),
            number(connected_at_ms),
        ))
    }

    pub fn heartbeat_message(&self, now_ms: i64) -> RelayMessage {
        RelayMessage::Heartbeat(build_heartbeat_message(
            self.config.workstation_id.clone(),
            self.config.boot_id.clone(),
            Number::from(self.pending.len() as u64),
            self.runtime.runtime_info(),
            number(now_ms.saturating_sub(self.config.started_at_ms).max(0)),
            number(now_ms),
        ))
    }

    fn status_report(&self, health: &RuntimeHealth, now_ms: i64) -> RelayMessage {
        RelayMessage::Status(build_status_report(
            self.config.workstation_id.clone(),
            self.runtime.runtime_info(),
            health.healthy,
            health.details.clone(),
            Number::from(self.pending.len() as u64),
            number(now_ms.saturating_sub(self.config.started_at_ms).max(0)),
            None,
            number(now_ms),
        ))
    }

    /// Probe runtime health off the outer I/O loop. The reply is delivered as
    /// [`RuntimeEvent::StatusCompleted`] so a slow `get_health()` cannot stall
    /// socket, timer, or stop handling.
    fn spawn_status_probe(&self) {
        let event_tx = self.event_tx.clone();
        let runtime = Arc::clone(&self.runtime);
        tokio::spawn(async move {
            let health = runtime.get_health().await;
            let _ = event_tx
                .send(RuntimeEvent::StatusCompleted { health })
                .await;
        });
    }

    pub async fn handle_inbound(
        &mut self,
        message: RelayMessage,
        now_ms: i64,
    ) -> Result<Vec<RelayMessage>, RunnerError> {
        match message {
            RelayMessage::ToolRequest(request)
                if request.envelope.workstation_id != self.config.workstation_id =>
            {
                Ok(Vec::new())
            }
            RelayMessage::ToolRequest(request) => self.handle_request((&request).into(), now_ms),
            RelayMessage::Cancel(cancel)
                if cancel.envelope.workstation_id != self.config.workstation_id =>
            {
                Ok(Vec::new())
            }
            RelayMessage::Cancel(cancel) => {
                self.handle_cancel(cancel.request_id, cancel.reason, now_ms)
            }
            RelayMessage::Status(status) if status.fields.query == Some(true) => {
                self.spawn_status_probe();
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn handle_request(
        &mut self,
        request: RuntimeRequest,
        now_ms: i64,
    ) -> Result<Vec<RelayMessage>, RunnerError> {
        if self.stopping {
            return Ok(vec![self.immediate_error(
                &request.request_id,
                CODE_LINK_STOPPING,
                true,
                "link is shutting down",
                DeliveryState::NotDelivered,
                now_ms,
                None,
            )]);
        }
        let request_id = request.request_id.clone();
        let timeout_ms =
            clamp_request_timeout(request.timeout_hint_ms(), self.config.request_timeout_ms);
        let slot = match self.pending.try_insert(request, timeout_ms, now_ms) {
            Ok(slot) => slot,
            Err(PendingInsertError::DuplicateRequest(request_id)) => {
                return Ok(vec![self.immediate_error(
                    &request_id,
                    CODE_DUPLICATE_REQUEST,
                    true,
                    "request already in flight",
                    DeliveryState::DeliveryUnknown,
                    now_ms,
                    None,
                )]);
            }
            Err(PendingInsertError::QueueFull { max_pending }) => {
                return Ok(vec![self.immediate_error(
                    &request_id,
                    CODE_QUEUE_FULL,
                    true,
                    &format!("pending queue full (max {max_pending})"),
                    DeliveryState::NotDelivered,
                    now_ms,
                    None,
                )]);
            }
        };

        let serving_generation = match self.runtime.begin_request_lease(&slot.request.request_id) {
            Ok(generation) => generation,
            Err(error) => {
                let _ = slot.claim_settle();
                self.pending.drop_if_same(&slot);
                return Ok(vec![self.immediate_error(
                    &request_id,
                    CODE_RUNTIME_GENERATION_DISPATCH_REJECTED,
                    false,
                    &error,
                    DeliveryState::NotDelivered,
                    now_ms,
                    None,
                )]);
            }
        };

        let timeout_tx = self.event_tx.clone();
        let timeout_slot = slot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_slot.timeout_ms)).await;
            let _ = timeout_tx
                .send(RuntimeEvent::RequestTimeout { slot: timeout_slot })
                .await;
        });

        let completion_tx = self.event_tx.clone();
        let runtime = Arc::clone(&self.runtime);
        let completion_slot = slot.clone();
        tokio::spawn(async move {
            let result = runtime
                .dispatch_request_with_lease(
                    completion_slot.request.clone(),
                    serving_generation.clone(),
                )
                .await;
            let _ = completion_tx
                .send(RuntimeEvent::DispatchCompleted {
                    slot: completion_slot,
                    serving_generation,
                    result,
                })
                .await;
        });
        Ok(Vec::new())
    }

    fn handle_cancel(
        &mut self,
        request_id: String,
        reason: Option<String>,
        now_ms: i64,
    ) -> Result<Vec<RelayMessage>, RunnerError> {
        let outcome = self.pending.cancel(&request_id);
        let accepted = matches!(outcome, super::request_core::CancelOutcome::Accepted(_));
        if accepted {
            let runtime = Arc::clone(&self.runtime);
            let runtime_request_id = request_id.clone();
            let runtime_reason = reason.clone().unwrap_or_else(|| "edge_cancel".to_owned());
            tokio::spawn(async move {
                runtime
                    .cancel_request(&runtime_request_id, &runtime_reason)
                    .await;
            });
        }
        let ack_reason = if accepted {
            reason
        } else {
            Some("no in-flight request".to_owned())
        };
        Ok(vec![RelayMessage::CancelAck(build_cancel_ack_message(
            self.config.workstation_id.clone(),
            request_id,
            accepted,
            number(now_ms),
            ack_reason,
        ))])
    }

    pub async fn next_runtime_output(
        &mut self,
        now_ms: i64,
    ) -> Result<Vec<RelayMessage>, RunnerError> {
        self.next_runtime_output_with_now(|| now_ms).await
    }

    /// Await one runtime event and timestamp its settlement only after it is
    /// actually observed. The outer I/O loop uses this form so a future that
    /// waits across multiple socket/timer turns never reuses a stale timestamp.
    pub async fn next_runtime_output_with_now<F>(
        &mut self,
        now_ms: F,
    ) -> Result<Vec<RelayMessage>, RunnerError>
    where
        F: FnOnce() -> i64,
    {
        let event = self
            .event_rx
            .recv()
            .await
            .ok_or(RunnerError::RuntimeEventChannelClosed)?;
        self.handle_runtime_event(event, now_ms())
    }

    fn handle_runtime_event(
        &mut self,
        event: RuntimeEvent,
        now_ms: i64,
    ) -> Result<Vec<RelayMessage>, RunnerError> {
        match event {
            RuntimeEvent::RequestTimeout { slot } => {
                let Some(slot) = self.pending.timeout_if_same(&slot) else {
                    return Ok(Vec::new());
                };
                let request_id = slot.request.request_id.clone();
                Ok(vec![self.immediate_error(
                    &request_id,
                    CODE_REQUEST_TIMEOUT,
                    true,
                    &format!(
                        "request exceeded {}ms local budget; execution state unknown",
                        slot.timeout_ms
                    ),
                    DeliveryState::DeliveryUnknown,
                    now_ms,
                    Some(json!({"timeout_ms": slot.timeout_ms})),
                )])
            }
            RuntimeEvent::DispatchCompleted {
                slot,
                serving_generation,
                result,
            } => {
                // Timeout/cancel/hard-stop may have already won this race. The
                // generation-aware runtime still completes its authoritative
                // lease when dispatch finishes; the runner only drops the late
                // correlated response.
                if slot.is_settled() {
                    return Ok(Vec::new());
                }
                let request_id = slot.request.request_id.clone();
                if !slot.claim_settle() {
                    return Ok(Vec::new());
                }
                self.pending.drop_if_same(&slot);
                Ok(vec![self.result_message(
                    request_id,
                    result,
                    serving_generation,
                    now_ms,
                )])
            }
            RuntimeEvent::StatusCompleted { health } => {
                Ok(vec![self.status_report(&health, now_ms)])
            }
        }
    }

    /// Reject new requests without disturbing work that is already in flight.
    /// The outer loop uses this before its bounded graceful-drain window.
    pub fn mark_stopping(&mut self) {
        self.stopping = true;
    }

    /// Settle every still-pending request after a hard stop/drain deadline.
    pub fn reject_pending(&mut self, now_ms: i64) -> Result<Vec<RelayMessage>, RunnerError> {
        let mut outbound = Vec::new();
        for slot in self.pending.reject_all() {
            let request_id = slot.request.request_id.clone();
            outbound.push(self.immediate_error(
                &request_id,
                CODE_LINK_STOPPING,
                true,
                "link is shutting down",
                DeliveryState::NotDelivered,
                now_ms,
                None,
            ));
        }
        Ok(outbound)
    }

    /// Backward-compatible immediate-stop helper for callers that do not own a
    /// graceful drain window.
    pub fn begin_stopping(&mut self, now_ms: i64) -> Result<Vec<RelayMessage>, RunnerError> {
        self.mark_stopping();
        self.reject_pending(now_ms)
    }

    /// Canonical Node-parity reply for a bounded inbound frame whose
    /// correlation id was recoverable before the byte gate rejected it.
    pub fn oversized_inbound_error(
        &self,
        request_id: impl Into<String>,
        size_bytes: usize,
        max_bytes: usize,
        now_ms: i64,
    ) -> RelayMessage {
        RelayMessage::ToolError(build_tool_error_message(
            self.config.workstation_id.clone(),
            request_id,
            "payload_too_large",
            false,
            "inbound frame exceeds maxFrameBytes",
            Some(json!({"size_bytes": size_bytes, "max": max_bytes})),
            Some(DeliveryState::NotDelivered),
            number(now_ms),
            None,
        ))
    }

    pub async fn route_transport_actions(
        &mut self,
        core: &LinkTransportCore,
        actions: Vec<TransportAction>,
        now_ms: i64,
    ) -> Result<Vec<TransportAction>, RunnerError> {
        let mut routed = Vec::new();
        for action in actions {
            match action {
                TransportAction::Inbound { message, .. } => {
                    let messages = self.handle_inbound(*message, now_ms).await?;
                    routed.extend(self.messages_to_actions(core, messages)?);
                }
                TransportAction::HeartbeatDue { .. } => {
                    routed.extend(
                        self.messages_to_actions(core, vec![self.heartbeat_message(now_ms)])?,
                    );
                }
                other => routed.push(other),
            }
        }
        Ok(routed)
    }

    pub async fn next_runtime_actions(
        &mut self,
        core: &LinkTransportCore,
        now_ms: i64,
    ) -> Result<Vec<TransportAction>, RunnerError> {
        self.next_runtime_actions_with_now(core, || now_ms).await
    }

    pub async fn next_runtime_actions_with_now<F>(
        &mut self,
        core: &LinkTransportCore,
        now_ms: F,
    ) -> Result<Vec<TransportAction>, RunnerError>
    where
        F: FnOnce() -> i64,
    {
        let messages = self.next_runtime_output_with_now(now_ms).await?;
        self.messages_to_actions(core, messages)
    }

    fn messages_to_actions(
        &self,
        core: &LinkTransportCore,
        messages: Vec<RelayMessage>,
    ) -> Result<Vec<TransportAction>, RunnerError> {
        let mut actions = Vec::new();
        for message in messages {
            actions.extend(core.send_outbound(message)?);
        }
        Ok(actions)
    }

    fn result_message(
        &self,
        request_id: String,
        result: RuntimeToolResult,
        serving_generation: String,
        now_ms: i64,
    ) -> RelayMessage {
        match result {
            RuntimeToolResult::Success { result } => {
                RelayMessage::ToolResult(build_tool_result_message(
                    self.config.workstation_id.clone(),
                    request_id,
                    result,
                    number(now_ms),
                    Some(serving_generation),
                    Some(self.runtime.name().to_owned()),
                ))
            }
            RuntimeToolResult::Failure {
                code,
                retryable,
                message,
                details,
            } => RelayMessage::ToolError(build_tool_error_message(
                self.config.workstation_id.clone(),
                request_id,
                code,
                retryable,
                message,
                details,
                Some(DeliveryState::Delivered),
                number(now_ms),
                Some(serving_generation),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn immediate_error(
        &self,
        request_id: &str,
        code: &str,
        retryable: bool,
        message: &str,
        delivery_state: DeliveryState,
        now_ms: i64,
        details: Option<Value>,
    ) -> RelayMessage {
        RelayMessage::ToolError(build_tool_error_message(
            self.config.workstation_id.clone(),
            request_id,
            code,
            retryable,
            message,
            details,
            Some(delivery_state),
            number(now_ms),
            None,
        ))
    }
}

fn number(value: i64) -> Number {
    Number::from(value.max(0))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Number, json};

    use super::{LinkRunnerCore, RunnerConfig};
    use crate::link::local_mcp::{LinkRuntimeTransport, RuntimeHealth, RuntimeToolResult};
    use crate::link::request_core::RuntimeRequest;
    use crate::relay::protocol::{
        CancelMessage, DeliveryState, OptionalNullable, RelayEnvelope, RelayMessage,
        RuntimeContractInfo, StatusFields, StatusMessage, ToolRequestMessage,
    };

    struct MockRuntime {
        generation: Mutex<Option<String>>,
        delay_ms: u64,
        health_delay_ms: u64,
        result: RuntimeToolResult,
        cancels: Mutex<Vec<String>>,
        health: RuntimeHealth,
    }

    impl MockRuntime {
        fn new(generation: &str, delay_ms: u64, result: RuntimeToolResult) -> Self {
            Self {
                generation: Mutex::new(Some(generation.to_owned())),
                delay_ms,
                health_delay_ms: 0,
                result,
                cancels: Mutex::new(Vec::new()),
                health: RuntimeHealth {
                    healthy: true,
                    details: None,
                },
            }
        }

        fn set_generation(&self, generation: Option<&str>) {
            *self
                .generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = generation.map(str::to_owned);
        }

        fn with_health_delay(mut self, health_delay_ms: u64) -> Self {
            self.health_delay_ms = health_delay_ms;
            self
        }
    }

    impl LinkRuntimeTransport for MockRuntime {
        fn name(&self) -> &str {
            "mock-runtime"
        }

        fn runtime_info(&self) -> RuntimeContractInfo {
            RuntimeContractInfo {
                runtime_version: "test".to_owned(),
                runtime_commit: None,
                runtime_generation: self
                    .generation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                contract_epoch: Number::from(2),
                contract_hash: Some("sha256:test".to_owned()),
                herdr_version: None,
                herdr_protocol: None,
            }
        }

        async fn dispatch_request(&self, _request: RuntimeRequest) -> RuntimeToolResult {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            self.result.clone()
        }

        async fn cancel_request(&self, request_id: &str, _reason: &str) {
            let request_id = request_id.to_owned();
            self.cancels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request_id);
        }

        async fn get_health(&self) -> RuntimeHealth {
            if self.health_delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.health_delay_ms)).await;
            }
            self.health.clone()
        }
    }

    fn request_message(id: &str, timeout_ms: u64) -> RelayMessage {
        RelayMessage::ToolRequest(ToolRequestMessage {
            envelope: RelayEnvelope::new("ws1"),
            request_id: id.to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: Some(Map::new()),
            timeout_ms: Some(Number::from(timeout_ms)),
            contract_epoch: Some(Number::from(2)),
            contract_hash: Some("sha256:test".to_owned()),
            idempotency_key: None,
            trace: None,
        })
    }

    fn runner(runtime: Arc<MockRuntime>, request_timeout_ms: u64) -> LinkRunnerCore<MockRuntime> {
        let mut config = RunnerConfig::new("ws1", "boot1", 1_000);
        config.request_timeout_ms = request_timeout_ms;
        config.max_pending = 2;
        LinkRunnerCore::new(config, runtime)
    }

    #[tokio::test]
    async fn successful_dispatch_settles_pending_and_generation_once() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            0,
            RuntimeToolResult::Success {
                result: Some(json!({"ok": true})),
            },
        ));
        let mut runner = runner(runtime, 1_000);
        assert!(
            runner
                .handle_inbound(request_message("r1", 500), 1_100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(runner.active_requests(), 1);

        let outbound = runner.next_runtime_output(1_120).await.unwrap();
        assert_eq!(runner.active_requests(), 0);
        match outbound.as_slice() {
            [RelayMessage::ToolResult(result)] => {
                assert_eq!(result.request_id, "r1");
                assert_eq!(result.result, Some(json!({"ok": true})));
                assert_eq!(
                    result.runtime_generation,
                    OptionalNullable::Value("gen-a".to_owned())
                );
                assert_eq!(
                    result.transport_name,
                    OptionalNullable::Value("mock-runtime".to_owned())
                );
            }
            other => panic!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_wins_and_late_completion_is_dropped_without_fence_leak() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            80,
            RuntimeToolResult::Success {
                result: Some(json!({"late": true})),
            },
        ));
        let mut runner = runner(runtime, 20);
        runner
            .handle_inbound(request_message("r1", 20), 1_100)
            .await
            .unwrap();
        let outbound = runner.next_runtime_output(1_120).await.unwrap();
        assert_eq!(runner.active_requests(), 0);
        assert!(matches!(
            outbound.as_slice(),
            [RelayMessage::ToolError(error)]
                if error.code == "request_timeout"
                    && error.delivery_state == Some(DeliveryState::DeliveryUnknown)
        ));
        let late = runner.next_runtime_output(1_200).await.unwrap();
        assert!(late.is_empty());
    }

    #[tokio::test]
    async fn cancel_releases_fence_and_notifies_only_matching_runtime_request() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            80,
            RuntimeToolResult::Success { result: None },
        ));
        let mut runner = runner(Arc::clone(&runtime), 1_000);
        runner
            .handle_inbound(request_message("r1", 500), 1_100)
            .await
            .unwrap();
        let outbound = runner
            .handle_inbound(
                RelayMessage::Cancel(CancelMessage {
                    envelope: RelayEnvelope::new("ws1"),
                    request_id: "r1".to_owned(),
                    reason: Some("stop".to_owned()),
                }),
                1_110,
            )
            .await
            .unwrap();
        assert_eq!(runner.active_requests(), 0);
        assert!(matches!(
            outbound.as_slice(),
            [RelayMessage::CancelAck(ack)] if ack.accepted
        ));
        tokio::task::yield_now().await;
        assert_eq!(
            runtime
                .cancels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &["r1".to_owned()]
        );
        assert!(runner.next_runtime_output(1_200).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn request_lease_generation_is_used_for_the_correlated_result() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-b",
            0,
            RuntimeToolResult::Success {
                result: Some(json!({"ok": true})),
            },
        ));
        let mut runner = runner(runtime, 1_000);
        runner
            .handle_inbound(request_message("r1", 500), 1_100)
            .await
            .unwrap();
        let outbound = runner.next_runtime_output(1_120).await.unwrap();
        assert_eq!(runner.active_requests(), 0);
        assert!(matches!(
            outbound.as_slice(),
            [RelayMessage::ToolResult(result)]
                if result.request_id == "r1"
                    && result.runtime_generation == OptionalNullable::Value("gen-b".to_owned())
        ));
    }

    #[tokio::test]
    async fn missing_generation_lease_fails_only_the_request_and_keeps_link_alive() {
        let mock = Arc::new(MockRuntime::new(
            "gen-a",
            0,
            RuntimeToolResult::Success {
                result: Some(json!({"unsafe": true})),
            },
        ));
        mock.set_generation(None);
        let mut runner = runner(mock, 1_000);
        let outbound = runner
            .handle_inbound(request_message("r1", 500), 1_100)
            .await
            .unwrap();
        assert_eq!(runner.active_requests(), 0);
        assert!(matches!(
            outbound.as_slice(),
            [RelayMessage::ToolError(error)]
                if error.request_id == "r1"
                    && error.code == "runtime_generation_dispatch_rejected"
                    && !error.retryable
                    && error.delivery_state == Some(DeliveryState::NotDelivered)
        ));
    }

    #[tokio::test]
    async fn queue_full_error_keeps_the_rejected_request_correlation() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            100,
            RuntimeToolResult::Success { result: None },
        ));
        let mut runner = runner(runtime, 1_000);
        runner
            .handle_inbound(request_message("r1", 500), 1_100)
            .await
            .unwrap();
        runner
            .handle_inbound(request_message("r2", 500), 1_101)
            .await
            .unwrap();

        let rejected = runner
            .handle_inbound(request_message("r3", 500), 1_102)
            .await
            .unwrap();
        assert!(matches!(
            rejected.as_slice(),
            [RelayMessage::ToolError(error)]
                if error.request_id == "r3"
                    && error.code == "request_queue_full"
                    && error.delivery_state == Some(DeliveryState::NotDelivered)
        ));
        assert_eq!(runner.active_requests(), 2);
    }

    #[tokio::test]
    async fn foreign_workstation_request_and_cancel_are_ignored() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            100,
            RuntimeToolResult::Success { result: None },
        ));
        let mut runner = runner(runtime, 1_000);
        let mut foreign_request = match request_message("r1", 500) {
            RelayMessage::ToolRequest(request) => request,
            _ => unreachable!(),
        };
        foreign_request.envelope = RelayEnvelope::new("other-workstation");
        assert!(
            runner
                .handle_inbound(RelayMessage::ToolRequest(foreign_request), 1_100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(runner.active_requests(), 0);

        assert!(
            runner
                .handle_inbound(
                    RelayMessage::Cancel(CancelMessage {
                        envelope: RelayEnvelope::new("other-workstation"),
                        request_id: "r1".to_owned(),
                        reason: Some("stop".to_owned()),
                    }),
                    1_101,
                )
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn duplicate_queue_stopping_heartbeat_and_status_are_canonical() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            100,
            RuntimeToolResult::Success { result: None },
        ));
        let mut runner = runner(runtime, 1_000);
        runner
            .handle_inbound(request_message("r1", 500), 1_100)
            .await
            .unwrap();
        let duplicate = runner
            .handle_inbound(request_message("r1", 500), 1_101)
            .await
            .unwrap();
        assert!(matches!(
            duplicate.as_slice(),
            [RelayMessage::ToolError(error)] if error.code == "duplicate_request"
        ));

        let heartbeat = runner.heartbeat_message(1_200);
        assert!(matches!(
            heartbeat,
            RelayMessage::Heartbeat(message) if message.active_requests == Number::from(1)
        ));
        let inbound = runner
            .handle_inbound(
                RelayMessage::Status(StatusMessage {
                    envelope: RelayEnvelope::new("ws1"),
                    fields: StatusFields {
                        query: Some(true),
                        ..StatusFields::default()
                    },
                }),
                1_200,
            )
            .await
            .unwrap();
        assert!(
            inbound.is_empty(),
            "status query must spawn a probe instead of awaiting health"
        );
        let status = runner.next_runtime_output(1_200).await.unwrap();
        assert!(matches!(
            status.as_slice(),
            [RelayMessage::Status(message)] if message.fields.healthy == Some(true)
        ));

        runner.mark_stopping();
        assert!(runner.stopping());
        assert_eq!(
            runner.active_requests(),
            1,
            "graceful drain keeps in-flight work"
        );
        let rejected = runner
            .handle_inbound(request_message("new", 500), 1_220)
            .await
            .unwrap();
        assert!(matches!(
            rejected.as_slice(),
            [RelayMessage::ToolError(error)] if error.code == "link_stopping"
        ));
        let stopping = runner.reject_pending(1_230).unwrap();
        assert_eq!(runner.active_requests(), 0);
        assert!(matches!(
            stopping.as_slice(),
            [RelayMessage::ToolError(error)] if error.code == "link_stopping"
        ));
    }

    #[test]
    fn oversized_inbound_error_matches_node_correlation_contract() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            0,
            RuntimeToolResult::Success { result: None },
        ));
        let runner = runner(runtime, 1_000);
        let message = runner.oversized_inbound_error("too-big", 300_000, 262_144, 1_250);
        assert!(matches!(
            message,
            RelayMessage::ToolError(error)
                if error.request_id == "too-big"
                    && error.code == "payload_too_large"
                    && !error.retryable
                    && error.delivery_state == Some(DeliveryState::NotDelivered)
                    && error.details == Some(json!({"size_bytes": 300_000, "max": 262_144}))
                    && error.served_at_ms == Some(Number::from(1_250))
        ));
    }

    #[tokio::test]
    async fn runtime_settlement_clock_is_sampled_after_the_wait() {
        let runtime = Arc::new(MockRuntime::new(
            "gen-a",
            20,
            RuntimeToolResult::Success {
                result: Some(json!({"ok": true})),
            },
        ));
        let mut runner = runner(runtime, 1_000);
        runner
            .handle_inbound(request_message("clocked", 500), 1_100)
            .await
            .unwrap();
        let outbound = runner.next_runtime_output_with_now(|| 9_999).await.unwrap();
        assert!(matches!(
            outbound.as_slice(),
            [RelayMessage::ToolResult(result)]
                if result.request_id == "clocked"
                    && result.served_at_ms == Number::from(9_999)
        ));
    }

    fn status_query() -> RelayMessage {
        RelayMessage::Status(StatusMessage {
            envelope: RelayEnvelope::new("ws1"),
            fields: StatusFields {
                query: Some(true),
                ..StatusFields::default()
            },
        })
    }

    #[tokio::test]
    async fn slow_status_health_does_not_block_inbound_handling() {
        let runtime = Arc::new(
            MockRuntime::new("gen-a", 0, RuntimeToolResult::Success { result: None })
                .with_health_delay(5_000),
        );
        let mut runner = runner(runtime, 1_000);
        let started = std::time::Instant::now();
        let inbound = runner.handle_inbound(status_query(), 1_200).await.unwrap();
        assert!(inbound.is_empty());
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "status query must not await a slow get_health on the inbound path"
        );
    }
}
