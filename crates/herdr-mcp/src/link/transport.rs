//! Socket-independent Rust Link transport reactor.
//!
//! This is the first transport slice above the Batch 4/5 reliability kernels.
//! It owns socket-attempt identity, Relay frame direction, handshake/reconnect
//! event ordering, and heartbeat/silence commands, but deliberately owns no
//! credential, concrete WebSocket implementation, runtime dispatch, daemon,
//! service mutation, or production activation.
//!
//! A future I/O driver executes [`TransportAction`] values and feeds observed
//! socket/timer events back into [`LinkTransportCore`]. Attempt ids fence stale
//! callbacks exactly like the Node client checks `this.ws === ws` before acting.

use crate::link::backoff::ExponentialBackoff;
use crate::link::heartbeat::{heartbeat_eligible, silence_check_interval_ms, silence_expired};
use crate::link::lifecycle::{
    ConnectionPhase, LifecycleAction, LifecycleError, LinkLifecycle, ReconnectSchedule,
};
use crate::link::policy::{LinkExitKind, WS_CLOSE_NORMAL};
use crate::relay::protocol::{HelloAckOutcome, HelloMessage, OptionalNullable, RelayMessage};
use crate::relay::validation::RelayValidationOptions;
use crate::relay::wire::{
    RelayWireError, build_compact_oversized_error, decode_relay_frame, encode_relay_message,
};
use serde_json::Value;
use std::io::{self, Write};

pub const LINK_DEFAULT_HEARTBEAT_MS: i64 = 30_000;
pub const LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS: i64 = 10_000;
pub const LINK_DEFAULT_MAX_FRAME_BYTES: usize = 262_144;
pub const LINK_DEFAULT_MAX_SILENCE_MS: i64 = 90_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAttemptId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportConfig {
    pub heartbeat_ms: i64,
    pub handshake_timeout_ms: i64,
    pub max_frame_bytes: usize,
    pub max_silence_ms: i64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            heartbeat_ms: LINK_DEFAULT_HEARTBEAT_MS,
            handshake_timeout_ms: LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS,
            max_frame_bytes: LINK_DEFAULT_MAX_FRAME_BYTES,
            max_silence_ms: LINK_DEFAULT_MAX_SILENCE_MS,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportAction {
    OpenSocket {
        attempt_id: SocketAttemptId,
    },
    SendFrame {
        attempt_id: SocketAttemptId,
        frame: String,
    },
    ArmHandshakeTimeout {
        attempt_id: SocketAttemptId,
        delay_ms: i64,
    },
    CancelHandshakeTimeout {
        attempt_id: SocketAttemptId,
    },
    ScheduleReconnect(ReconnectSchedule),
    CancelReconnectWait,
    StartOnlineTimers {
        heartbeat_ms: i64,
        silence_check_ms: f64,
    },
    StopOnlineTimers,
    CloseSocket {
        attempt_id: SocketAttemptId,
        code: u16,
        reason: &'static str,
    },
    TerminateSocket {
        attempt_id: SocketAttemptId,
    },
    Online {
        attempt_id: SocketAttemptId,
        connected_at_ms: i64,
    },
    Disconnected {
        attempt_id: SocketAttemptId,
        code: u16,
        reason: String,
    },
    HeartbeatDue {
        attempt_id: SocketAttemptId,
    },
    Inbound {
        attempt_id: SocketAttemptId,
        message: Box<RelayMessage>,
    },
    OversizedInbound {
        attempt_id: SocketAttemptId,
        request_id: Option<String>,
        size_bytes: usize,
        max_bytes: usize,
    },
    InboundRejected {
        attempt_id: SocketAttemptId,
        code: String,
        reason: String,
    },
    SocketErrorObserved {
        attempt_id: SocketAttemptId,
    },
    Exit(LinkExitKind),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportError {
    Lifecycle(LifecycleError),
    Wire(RelayWireError),
    WorkstationMismatch { expected: String, observed: String },
}

impl From<LifecycleError> for TransportError {
    fn from(value: LifecycleError) -> Self {
        Self::Lifecycle(value)
    }
}

impl From<RelayWireError> for TransportError {
    fn from(value: RelayWireError) -> Self {
        Self::Wire(value)
    }
}

#[derive(Debug, Clone)]
pub struct LinkTransportCore {
    workstation_id: String,
    config: TransportConfig,
    lifecycle: LinkLifecycle,
    next_attempt_id: u64,
    active_attempt: Option<SocketAttemptId>,
    socket_open: bool,
    connected_at_ms: Option<i64>,
    last_edge_seen_ms: Option<i64>,
    silence_recycle_requested: bool,
}

impl LinkTransportCore {
    pub fn new(
        workstation_id: impl Into<String>,
        max_reconnect_attempts: Option<u64>,
        backoff: ExponentialBackoff,
        config: TransportConfig,
    ) -> Self {
        Self {
            workstation_id: workstation_id.into(),
            config,
            lifecycle: LinkLifecycle::new(max_reconnect_attempts, backoff),
            next_attempt_id: 0,
            active_attempt: None,
            socket_open: false,
            connected_at_ms: None,
            last_edge_seen_ms: None,
            silence_recycle_requested: false,
        }
    }

    pub fn phase(&self) -> ConnectionPhase {
        self.lifecycle.phase()
    }

    pub fn stopped(&self) -> bool {
        self.lifecycle.stopped()
    }

    pub fn active_attempt(&self) -> Option<SocketAttemptId> {
        self.active_attempt
    }

    pub fn reconnect_attempt(&self) -> u64 {
        self.lifecycle.reconnect_attempt()
    }

    pub fn connected_at_ms(&self) -> Option<i64> {
        self.connected_at_ms
    }

    pub fn last_edge_seen_ms(&self) -> Option<i64> {
        self.last_edge_seen_ms
    }

    pub fn start(&mut self) -> Result<Vec<TransportAction>, TransportError> {
        let action = self.lifecycle.start()?;
        self.translate_lifecycle(action)
    }

    /// Report a socket factory/listener-attachment failure before `open`.
    pub fn socket_connect_failed(
        &mut self,
        attempt_id: SocketAttemptId,
        now_ms: i64,
        rng_sample: f64,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if !self.is_active(attempt_id) {
            return Ok(Vec::new());
        }
        self.active_attempt = None;
        self.socket_open = false;
        let action = self
            .lifecycle
            .socket_failed_before_open(now_ms as f64, rng_sample)?;
        self.translate_lifecycle(action)
    }

    /// Transition an active socket into handshake and emit the canonical hello.
    pub fn socket_opened(
        &mut self,
        attempt_id: SocketAttemptId,
        hello: HelloMessage,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if !self.is_active(attempt_id) {
            return Ok(Vec::new());
        }
        if hello.envelope.workstation_id != self.workstation_id {
            return Err(TransportError::WorkstationMismatch {
                expected: self.workstation_id.clone(),
                observed: hello.envelope.workstation_id,
            });
        }
        let frame = encode_relay_message(
            &RelayMessage::Hello(hello),
            Some(&self.validation_options()),
        )?;
        self.lifecycle.socket_opened()?;
        self.socket_open = true;
        Ok(vec![
            TransportAction::SendFrame { attempt_id, frame },
            TransportAction::ArmHandshakeTimeout {
                attempt_id,
                delay_ms: self.config.handshake_timeout_ms,
            },
        ])
    }

    /// Observe one bounded text frame from the active socket.
    ///
    /// As in Node, any non-oversized frame counts as edge activity before JSON
    /// validation; malformed frames therefore suppress the silence watchdog but
    /// do not enter typed protocol state.
    pub fn frame_received(
        &mut self,
        attempt_id: SocketAttemptId,
        raw: &str,
        now_ms: i64,
        rng_sample: f64,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if !self.is_active(attempt_id) {
            return Ok(Vec::new());
        }
        let size_bytes = raw.len();
        if size_bytes > self.config.max_frame_bytes {
            return Ok(vec![TransportAction::OversizedInbound {
                attempt_id,
                request_id: extract_request_id(raw),
                size_bytes,
                max_bytes: self.config.max_frame_bytes,
            }]);
        }
        self.last_edge_seen_ms = Some(now_ms);
        let message = match decode_relay_frame(raw, Some(&self.validation_options())) {
            Ok(message) => message,
            Err(error) => {
                return Ok(vec![TransportAction::InboundRejected {
                    attempt_id,
                    code: error.code,
                    reason: error.reason,
                }]);
            }
        };

        match message {
            RelayMessage::HelloAck(ack) => {
                if self.lifecycle.phase() != ConnectionPhase::Handshake {
                    return Ok(Vec::new());
                }
                match ack.outcome {
                    HelloAckOutcome::Success { .. } => {
                        self.lifecycle.hello_ack_succeeded()?;
                        self.connected_at_ms = Some(now_ms);
                        self.silence_recycle_requested = false;
                        Ok(vec![
                            TransportAction::CancelHandshakeTimeout { attempt_id },
                            TransportAction::Online {
                                attempt_id,
                                connected_at_ms: now_ms,
                            },
                            TransportAction::StartOnlineTimers {
                                heartbeat_ms: self.config.heartbeat_ms,
                                silence_check_ms: silence_check_interval_ms(
                                    self.config.heartbeat_ms as f64,
                                    self.config.max_silence_ms as f64,
                                ),
                            },
                        ])
                    }
                    HelloAckOutcome::Failure { code, message } => {
                        let _ = writeln!(
                            io::stderr(),
                            "[herdr-link-daemon] warn hello_ack refused code={} message={}",
                            code,
                            message
                        );
                        let action = self.lifecycle.hello_ack_refused(
                            Some(&code),
                            now_ms as f64,
                            rng_sample,
                        )?;
                        self.active_attempt = None;
                        self.socket_open = false;
                        let mut actions = vec![
                            TransportAction::CancelHandshakeTimeout { attempt_id },
                            TransportAction::TerminateSocket { attempt_id },
                        ];
                        actions.extend(self.translate_lifecycle(action)?);
                        Ok(actions)
                    }
                }
            }
            RelayMessage::ToolRequest(request) => {
                if request.envelope.workstation_id != self.workstation_id {
                    return Ok(Vec::new());
                }
                Ok(vec![TransportAction::Inbound {
                    attempt_id,
                    message: Box::new(RelayMessage::ToolRequest(request)),
                }])
            }
            RelayMessage::Cancel(cancel) => {
                if cancel.envelope.workstation_id != self.workstation_id {
                    return Ok(Vec::new());
                }
                Ok(vec![TransportAction::Inbound {
                    attempt_id,
                    message: Box::new(RelayMessage::Cancel(cancel)),
                }])
            }
            RelayMessage::Status(status) if status.fields.query == Some(true) => {
                Ok(vec![TransportAction::Inbound {
                    attempt_id,
                    message: Box::new(RelayMessage::Status(status)),
                }])
            }
            RelayMessage::Status(_) => Ok(Vec::new()),
            unexpected => Ok(vec![TransportAction::InboundRejected {
                attempt_id,
                code: "unexpected_inbound_kind".to_owned(),
                reason: format!(
                    "edge sent workstation-outbound kind {}",
                    unexpected.kind().as_str()
                ),
            }]),
        }
    }

    pub fn socket_error(&self, attempt_id: SocketAttemptId) -> Vec<TransportAction> {
        if self.is_active(attempt_id) {
            vec![TransportAction::SocketErrorObserved { attempt_id }]
        } else {
            Vec::new()
        }
    }

    pub fn socket_closed(
        &mut self,
        attempt_id: SocketAttemptId,
        code: u16,
        reason: impl Into<String>,
        now_ms: i64,
        rng_sample: f64,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if !self.is_active(attempt_id) {
            return Ok(Vec::new());
        }
        let phase = self.lifecycle.phase();
        let reason = reason.into();
        self.active_attempt = None;
        self.socket_open = false;
        self.last_edge_seen_ms = None;
        self.silence_recycle_requested = false;

        let mut actions = Vec::new();
        match phase {
            ConnectionPhase::Connecting => {
                let action = self
                    .lifecycle
                    .socket_failed_before_open(now_ms as f64, rng_sample)?;
                actions.extend(self.translate_lifecycle(action)?);
            }
            ConnectionPhase::Handshake => {
                actions.push(TransportAction::CancelHandshakeTimeout { attempt_id });
                let action = self
                    .lifecycle
                    .socket_failed_during_handshake(now_ms as f64, rng_sample)?;
                actions.extend(self.translate_lifecycle(action)?);
            }
            ConnectionPhase::Online => {
                actions.push(TransportAction::StopOnlineTimers);
                actions.push(TransportAction::Disconnected {
                    attempt_id,
                    code,
                    reason,
                });
                let action =
                    self.lifecycle
                        .online_socket_closed(code, now_ms as f64, rng_sample)?;
                actions.extend(self.translate_lifecycle(action)?);
            }
            ConnectionPhase::Closing => {
                actions.push(TransportAction::StopOnlineTimers);
                actions.push(TransportAction::CancelHandshakeTimeout { attempt_id });
                let finish = self.lifecycle.finish_close()?;
                actions.extend(self.translate_lifecycle(finish)?);
            }
            ConnectionPhase::Idle | ConnectionPhase::Reconnecting | ConnectionPhase::Closed => {}
        }
        Ok(actions)
    }

    pub fn handshake_timed_out(
        &mut self,
        attempt_id: SocketAttemptId,
        now_ms: i64,
        rng_sample: f64,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if !self.is_active(attempt_id) || self.lifecycle.phase() != ConnectionPhase::Handshake {
            return Ok(Vec::new());
        }
        let action = self
            .lifecycle
            .handshake_timed_out(now_ms as f64, rng_sample)?;
        self.active_attempt = None;
        self.socket_open = false;
        let mut actions = vec![TransportAction::TerminateSocket { attempt_id }];
        actions.extend(self.translate_lifecycle(action)?);
        Ok(actions)
    }

    pub fn reconnect_wait_elapsed(
        &mut self,
        expected_attempt: u64,
    ) -> Result<Vec<TransportAction>, TransportError> {
        if self.lifecycle.phase() != ConnectionPhase::Reconnecting
            || self.lifecycle.reconnect_attempt() != expected_attempt
        {
            return Ok(Vec::new());
        }
        let action = self.lifecycle.reconnect_wait_elapsed()?;
        self.translate_lifecycle(action)
    }

    pub fn heartbeat_tick(&self) -> Vec<TransportAction> {
        if heartbeat_eligible(
            self.lifecycle.phase(),
            self.lifecycle.stopped(),
            self.socket_open,
        ) && let Some(attempt_id) = self.active_attempt
        {
            return vec![TransportAction::HeartbeatDue { attempt_id }];
        }
        Vec::new()
    }

    pub fn silence_tick(&mut self, now_ms: i64) -> Vec<TransportAction> {
        if self.silence_recycle_requested
            || !heartbeat_eligible(
                self.lifecycle.phase(),
                self.lifecycle.stopped(),
                self.socket_open,
            )
            || !silence_expired(
                now_ms,
                self.last_edge_seen_ms,
                self.connected_at_ms,
                self.config.max_silence_ms,
            )
        {
            return Vec::new();
        }
        let Some(attempt_id) = self.active_attempt else {
            return Vec::new();
        };
        self.silence_recycle_requested = true;
        vec![
            TransportAction::StopOnlineTimers,
            TransportAction::TerminateSocket { attempt_id },
        ]
    }

    /// Encode one higher-layer outbound message onto the current active socket.
    /// Runtime request settlement is intentionally outside this first slice.
    pub fn send_outbound(
        &self,
        message: RelayMessage,
    ) -> Result<Vec<TransportAction>, TransportError> {
        let Some(attempt_id) = self.active_attempt else {
            return Ok(Vec::new());
        };
        if !self.socket_open {
            return Ok(Vec::new());
        }
        if message.workstation_id() != self.workstation_id {
            return Err(TransportError::WorkstationMismatch {
                expected: self.workstation_id.clone(),
                observed: message.workstation_id().to_owned(),
            });
        }
        let frame = match encode_relay_message(&message, Some(&self.validation_options())) {
            Ok(frame) => frame,
            Err(error) if error.code == "frame_too_large" => {
                let RelayMessage::ToolResult(result) = &message else {
                    return Ok(Vec::new());
                };
                let runtime_generation = match &result.runtime_generation {
                    OptionalNullable::Value(value) => Some(value.clone()),
                    OptionalNullable::Absent | OptionalNullable::Null => None,
                };
                let compact = RelayMessage::ToolError(build_compact_oversized_error(
                    &self.workstation_id,
                    &result.request_id,
                    runtime_generation,
                    result.served_at_ms.clone(),
                ));
                match encode_relay_message(&compact, Some(&self.validation_options())) {
                    Ok(frame) => frame,
                    Err(compact_error) if compact_error.code == "frame_too_large" => {
                        return Ok(Vec::new());
                    }
                    Err(compact_error) => return Err(compact_error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        Ok(vec![TransportAction::SendFrame { attempt_id, frame }])
    }

    pub fn request_close(&mut self) -> Result<Vec<TransportAction>, TransportError> {
        let previous_phase = self.lifecycle.phase();
        let action = self.lifecycle.request_close()?;
        let mut actions = Vec::new();
        match previous_phase {
            ConnectionPhase::Online => actions.push(TransportAction::StopOnlineTimers),
            ConnectionPhase::Handshake => {
                if let Some(attempt_id) = self.active_attempt {
                    actions.push(TransportAction::CancelHandshakeTimeout { attempt_id });
                }
            }
            _ => {}
        }

        match action {
            LifecycleAction::CancelReconnectWait => {
                actions.push(TransportAction::CancelReconnectWait);
                let finish = self.lifecycle.finish_close()?;
                actions.extend(self.translate_lifecycle(finish)?);
            }
            LifecycleAction::CloseSocket => {
                if let Some(attempt_id) = self.active_attempt {
                    if self.socket_open {
                        actions.push(TransportAction::CloseSocket {
                            attempt_id,
                            code: WS_CLOSE_NORMAL,
                            reason: "client shutdown",
                        });
                    } else {
                        actions.push(TransportAction::TerminateSocket { attempt_id });
                    }
                } else {
                    let finish = self.lifecycle.finish_close()?;
                    actions.extend(self.translate_lifecycle(finish)?);
                }
            }
            LifecycleAction::None => {
                let finish = self.lifecycle.finish_close()?;
                actions.extend(self.translate_lifecycle(finish)?);
            }
            LifecycleAction::Exit(kind) => actions.push(TransportAction::Exit(kind)),
            other => actions.extend(self.translate_lifecycle(other)?),
        }
        Ok(actions)
    }

    fn is_active(&self, attempt_id: SocketAttemptId) -> bool {
        self.active_attempt == Some(attempt_id)
    }

    fn validation_options(&self) -> RelayValidationOptions {
        RelayValidationOptions {
            max_frame_bytes: Some(self.config.max_frame_bytes),
            ..RelayValidationOptions::default()
        }
    }

    fn begin_socket_attempt(&mut self) -> TransportAction {
        self.next_attempt_id = self.next_attempt_id.saturating_add(1);
        let attempt_id = SocketAttemptId(self.next_attempt_id);
        self.active_attempt = Some(attempt_id);
        self.socket_open = false;
        TransportAction::OpenSocket { attempt_id }
    }

    fn translate_lifecycle(
        &mut self,
        action: LifecycleAction,
    ) -> Result<Vec<TransportAction>, TransportError> {
        Ok(match action {
            LifecycleAction::None => Vec::new(),
            LifecycleAction::ConnectNow => vec![self.begin_socket_attempt()],
            LifecycleAction::ScheduleReconnect(schedule) => {
                vec![TransportAction::ScheduleReconnect(schedule)]
            }
            LifecycleAction::Online => Vec::new(),
            LifecycleAction::CancelReconnectWait => vec![TransportAction::CancelReconnectWait],
            LifecycleAction::CloseSocket => self
                .active_attempt
                .map(|attempt_id| {
                    vec![TransportAction::CloseSocket {
                        attempt_id,
                        code: WS_CLOSE_NORMAL,
                        reason: "client shutdown",
                    }]
                })
                .unwrap_or_default(),
            LifecycleAction::Exit(kind) => vec![TransportAction::Exit(kind)],
        })
    }
}

fn extract_request_id(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()?
        .as_object()?
        .get("request_id")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS, LINK_DEFAULT_MAX_FRAME_BYTES, LinkTransportCore,
        SocketAttemptId, TransportAction, TransportConfig,
    };
    use crate::link::backoff::{BackoffOptions, ExponentialBackoff};
    use crate::link::lifecycle::ConnectionPhase;
    use crate::link::policy::{LinkExitKind, WS_CLOSE_SUPERSEDED};
    use crate::relay::protocol::{
        DeliveryState, HelloAckMessage, HelloAckOutcome, OptionalNullable, RelayEnvelope,
        RelayMessage, RuntimeContractInfo, StatusFields, StatusMessage, ToolResultMessage,
    };
    use crate::relay::wire::{build_hello_message, decode_relay_frame, encode_relay_message};
    use serde_json::{Number, json};

    fn runtime() -> RuntimeContractInfo {
        RuntimeContractInfo {
            runtime_version: "0.4.0-alpha.6".to_owned(),
            runtime_commit: Some("abc".to_owned()),
            runtime_generation: Some("g1".to_owned()),
            contract_epoch: Number::from(2),
            contract_hash: Some("sha256:test".to_owned()),
            herdr_version: Some("0.8.2".to_owned()),
            herdr_protocol: Some("20".to_owned()),
        }
    }

    fn hello() -> crate::relay::protocol::HelloMessage {
        build_hello_message(
            "ws1",
            "boot1",
            "0.4.0-alpha.6",
            vec!["herdr".to_owned()],
            runtime(),
            Number::from(1_000),
        )
    }

    fn core() -> LinkTransportCore {
        LinkTransportCore::new(
            "ws1",
            Some(3),
            ExponentialBackoff::new(BackoffOptions {
                base_ms: Some(1_000.0),
                max_ms: Some(60_000.0),
                factor: Some(2.0),
                jitter: Some(0.0),
            }),
            TransportConfig::default(),
        )
    }

    fn open_and_handshake(core: &mut LinkTransportCore) -> SocketAttemptId {
        let start = core.start().expect("start");
        let attempt_id = match start.as_slice() {
            [TransportAction::OpenSocket { attempt_id }] => *attempt_id,
            other => panic!("unexpected start actions: {other:?}"),
        };
        let opened = core.socket_opened(attempt_id, hello()).expect("open");
        assert!(matches!(
            opened.as_slice(),
            [
                TransportAction::SendFrame { .. },
                TransportAction::ArmHandshakeTimeout {
                    delay_ms: LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS,
                    ..
                }
            ]
        ));
        attempt_id
    }

    fn success_ack() -> String {
        encode_relay_message(
            &RelayMessage::HelloAck(HelloAckMessage {
                envelope: RelayEnvelope::new("ws1"),
                outcome: HelloAckOutcome::Success {
                    server_version: Some("edge".to_owned()),
                    edge_deployment_id: None,
                    capabilities: None,
                    reconnect: None,
                    resume: None,
                    completed: None,
                },
            }),
            None,
        )
        .expect("ack")
    }

    #[test]
    fn start_open_hello_ack_reaches_online_and_starts_timers() {
        let mut core = core();
        let attempt_id = open_and_handshake(&mut core);
        assert_eq!(core.phase(), ConnectionPhase::Handshake);

        let actions = core
            .frame_received(attempt_id, &success_ack(), 2_000, 0.5)
            .expect("ack frame");
        assert_eq!(core.phase(), ConnectionPhase::Online);
        assert_eq!(core.connected_at_ms(), Some(2_000));
        assert!(matches!(
            actions.as_slice(),
            [
                TransportAction::CancelHandshakeTimeout { .. },
                TransportAction::Online {
                    connected_at_ms: 2_000,
                    ..
                },
                TransportAction::StartOnlineTimers {
                    heartbeat_ms: 30_000,
                    silence_check_ms: 30_000.0,
                }
            ]
        ));
        assert_eq!(
            core.heartbeat_tick(),
            vec![TransportAction::HeartbeatDue { attempt_id }]
        );
    }

    #[test]
    fn handshake_drop_and_timeout_schedule_reconnect_without_double_close() {
        let mut dropped = core();
        let attempt = open_and_handshake(&mut dropped);
        let actions = dropped
            .socket_closed(attempt, 1006, "drop", 5_000, 0.5)
            .expect("close");
        assert_eq!(dropped.phase(), ConnectionPhase::Reconnecting);
        assert!(matches!(
            actions.as_slice(),
            [
                TransportAction::CancelHandshakeTimeout { .. },
                TransportAction::ScheduleReconnect(schedule)
            ] if schedule.attempt == 1 && schedule.delay_ms == 1_000.0
        ));

        let mut timed_out = core();
        let attempt = open_and_handshake(&mut timed_out);
        let actions = timed_out
            .handshake_timed_out(attempt, 5_000, 0.5)
            .expect("timeout");
        assert_eq!(timed_out.phase(), ConnectionPhase::Reconnecting);
        assert!(matches!(
            actions.as_slice(),
            [
                TransportAction::TerminateSocket { .. },
                TransportAction::ScheduleReconnect(schedule)
            ] if schedule.attempt == 1 && schedule.delay_ms == 1_000.0
        ));
        assert!(
            timed_out
                .socket_closed(attempt, 1006, "late close", 5_001, 0.5)
                .expect("stale close")
                .is_empty()
        );
    }

    #[test]
    fn fatal_refusal_and_superseded_close_exit_without_reconnect() {
        let mut refused = core();
        let attempt = open_and_handshake(&mut refused);
        let raw = encode_relay_message(
            &RelayMessage::HelloAck(HelloAckMessage {
                envelope: RelayEnvelope::new("ws1"),
                outcome: HelloAckOutcome::Failure {
                    code: "auth_rejected".to_owned(),
                    message: "no".to_owned(),
                },
            }),
            None,
        )
        .unwrap();
        let actions = refused.frame_received(attempt, &raw, 2_000, 0.5).unwrap();
        assert_eq!(refused.phase(), ConnectionPhase::Closed);
        assert!(matches!(
            actions.as_slice(),
            [
                TransportAction::CancelHandshakeTimeout { .. },
                TransportAction::TerminateSocket { .. },
                TransportAction::Exit(LinkExitKind::AuthRejected)
            ]
        ));

        let mut fenced = core();
        let attempt = open_and_handshake(&mut fenced);
        fenced
            .frame_received(attempt, &success_ack(), 2_000, 0.5)
            .unwrap();
        let actions = fenced
            .socket_closed(attempt, WS_CLOSE_SUPERSEDED, "newer", 3_000, 0.5)
            .unwrap();
        assert_eq!(fenced.phase(), ConnectionPhase::Closed);
        assert!(matches!(
            actions.as_slice(),
            [
                TransportAction::StopOnlineTimers,
                TransportAction::Disconnected { .. },
                TransportAction::Exit(LinkExitKind::Superseded)
            ]
        ));
    }

    #[test]
    fn reconnect_timer_creates_new_attempt_and_stale_events_are_fenced() {
        let mut core = core();
        let first = match core.start().unwrap().as_slice() {
            [TransportAction::OpenSocket { attempt_id }] => *attempt_id,
            other => panic!("unexpected: {other:?}"),
        };
        let scheduled = core.socket_connect_failed(first, 1_000, 0.5).unwrap();
        assert!(matches!(
            scheduled.as_slice(),
            [TransportAction::ScheduleReconnect(schedule)] if schedule.attempt == 1
        ));
        let next = core.reconnect_wait_elapsed(1).unwrap();
        let second = match next.as_slice() {
            [TransportAction::OpenSocket { attempt_id }] => *attempt_id,
            other => panic!("unexpected: {other:?}"),
        };
        assert_ne!(first, second);
        assert!(core.socket_opened(first, hello()).unwrap().is_empty());
        assert_eq!(core.active_attempt(), Some(second));
    }

    #[test]
    fn bounded_malformed_and_operational_frames_match_node_direction_policy() {
        let mut core = core();
        let attempt = open_and_handshake(&mut core);
        core.frame_received(attempt, &success_ack(), 2_000, 0.5)
            .unwrap();

        let malformed = core.frame_received(attempt, "{", 2_100, 0.5).unwrap();
        assert_eq!(core.last_edge_seen_ms(), Some(2_100));
        assert!(matches!(
            malformed.as_slice(),
            [TransportAction::InboundRejected { code, .. }] if code == "not_json"
        ));

        let status = RelayMessage::Status(StatusMessage {
            envelope: RelayEnvelope::new("ws1"),
            fields: StatusFields {
                query: Some(true),
                ..StatusFields::default()
            },
        });
        let raw = encode_relay_message(&status, None).unwrap();
        let inbound = core.frame_received(attempt, &raw, 2_200, 0.5).unwrap();
        assert!(matches!(
            inbound.as_slice(),
            [TransportAction::Inbound { message, .. }]
                if matches!(message.as_ref(), RelayMessage::Status(_))
        ));
    }

    #[test]
    fn oversized_frame_preserves_request_correlation_without_refreshing_silence() {
        let mut core = core();
        let attempt = open_and_handshake(&mut core);
        core.frame_received(attempt, &success_ack(), 2_000, 0.5)
            .unwrap();
        let before = core.last_edge_seen_ms();
        let raw = format!(
            "{{\"request_id\":\"r1\",\"pad\":\"{}\"}}",
            "x".repeat(LINK_DEFAULT_MAX_FRAME_BYTES)
        );
        let actions = core.frame_received(attempt, &raw, 2_100, 0.5).unwrap();
        assert_eq!(core.last_edge_seen_ms(), before);
        assert!(matches!(
            actions.as_slice(),
            [TransportAction::OversizedInbound {
                request_id: Some(request_id),
                ..
            }] if request_id == "r1"
        ));
    }

    #[test]
    fn oversized_outbound_result_falls_back_to_compact_correlated_error() {
        let mut core = LinkTransportCore::new(
            "ws1",
            Some(3),
            ExponentialBackoff::default(),
            TransportConfig {
                max_frame_bytes: 512,
                ..TransportConfig::default()
            },
        );
        let attempt = open_and_handshake(&mut core);
        core.frame_received(attempt, &success_ack(), 2_000, 0.5)
            .unwrap();

        let result = RelayMessage::ToolResult(ToolResultMessage {
            envelope: RelayEnvelope::new("ws1"),
            request_id: "r1".to_owned(),
            result: Some(json!({ "payload": "x".repeat(2_000) })),
            served_at_ms: Number::from(2_500),
            runtime_generation: OptionalNullable::Value("g1".to_owned()),
            transport_name: OptionalNullable::Value("local".to_owned()),
        });
        let actions = core.send_outbound(result).expect("fallback");
        let frame = match actions.as_slice() {
            [TransportAction::SendFrame { frame, .. }] => frame,
            other => panic!("unexpected actions: {other:?}"),
        };
        let decoded = decode_relay_frame(frame, None).expect("compact frame");
        let RelayMessage::ToolError(error) = decoded else {
            panic!("expected compact tool_error");
        };
        assert_eq!(error.request_id, "r1");
        assert_eq!(error.code, "response_too_large");
        assert!(!error.retryable);
        assert_eq!(error.delivery_state, Some(DeliveryState::Delivered));
        assert_eq!(error.served_at_ms, Some(Number::from(2_500)));
        assert_eq!(
            error.runtime_generation,
            OptionalNullable::Value("g1".to_owned())
        );
    }

    #[test]
    fn silence_recycle_is_single_shot_until_socket_close() {
        let mut core = core();
        let attempt = open_and_handshake(&mut core);
        core.frame_received(attempt, &success_ack(), 2_000, 0.5)
            .unwrap();
        assert!(core.silence_tick(92_000).is_empty());
        let recycle = core.silence_tick(92_001);
        assert_eq!(
            recycle,
            vec![
                TransportAction::StopOnlineTimers,
                TransportAction::TerminateSocket {
                    attempt_id: attempt
                },
            ]
        );
        assert!(core.silence_tick(200_000).is_empty());
    }

    #[test]
    fn graceful_close_during_backoff_cancels_wait_and_exits_stopped() {
        let mut core = core();
        let attempt = match core.start().unwrap().as_slice() {
            [TransportAction::OpenSocket { attempt_id }] => *attempt_id,
            other => panic!("unexpected: {other:?}"),
        };
        core.socket_connect_failed(attempt, 1_000, 0.5).unwrap();
        let actions = core.request_close().unwrap();
        assert_eq!(core.phase(), ConnectionPhase::Closed);
        assert_eq!(
            actions,
            vec![
                TransportAction::CancelReconnectWait,
                TransportAction::Exit(LinkExitKind::Stopped),
            ]
        );
    }
}
