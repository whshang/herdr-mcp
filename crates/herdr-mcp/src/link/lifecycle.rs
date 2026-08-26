//! Transport-independent workstation-link lifecycle state machine.
//!
//! This module mirrors the reconnect/fatal/close policy in `src/link/client.ts`
//! without owning sockets, timers, credentials, runtime dispatch, or process
//! lifecycle. Callers perform I/O and feed the resulting events back into this
//! state machine.

use super::backoff::ExponentialBackoff;
use super::policy::{
    LinkDirective, LinkExitKind, classify_handshake_timeout, classify_hello_ack_refusal,
    classify_socket_close,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    Idle,
    Connecting,
    Handshake,
    Online,
    Reconnecting,
    Closing,
    Closed,
}

impl ConnectionPhase {
    pub const ALL: [Self; 7] = [
        Self::Idle,
        Self::Connecting,
        Self::Handshake,
        Self::Online,
        Self::Reconnecting,
        Self::Closing,
        Self::Closed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Handshake => "handshake",
            Self::Online => "online",
            Self::Reconnecting => "reconnecting",
            Self::Closing => "closing",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconnectSchedule {
    pub attempt: u64,
    pub delay_ms: f64,
    pub reconnect_at_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LifecycleAction {
    None,
    ConnectNow,
    ScheduleReconnect(ReconnectSchedule),
    Online,
    CancelReconnectWait,
    CloseSocket,
    Exit(LinkExitKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleError {
    pub phase: ConnectionPhase,
    pub event: &'static str,
}

impl LifecycleError {
    const fn invalid(phase: ConnectionPhase, event: &'static str) -> Self {
        Self { phase, event }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkLifecycle {
    phase: ConnectionPhase,
    stopped: bool,
    reconnect_attempt: u64,
    reconnect_at_ms: Option<f64>,
    max_reconnect_attempts: Option<u64>,
    backoff: ExponentialBackoff,
}

impl LinkLifecycle {
    pub fn new(max_reconnect_attempts: Option<u64>, backoff: ExponentialBackoff) -> Self {
        Self {
            phase: ConnectionPhase::Idle,
            stopped: false,
            reconnect_attempt: 0,
            reconnect_at_ms: None,
            max_reconnect_attempts,
            backoff,
        }
    }

    pub const fn phase(&self) -> ConnectionPhase {
        self.phase
    }

    pub const fn stopped(&self) -> bool {
        self.stopped
    }

    pub const fn reconnect_attempt(&self) -> u64 {
        self.reconnect_attempt
    }

    pub const fn reconnect_at_ms(&self) -> Option<f64> {
        self.reconnect_at_ms
    }

    pub fn backoff_attempt(&self) -> u64 {
        self.backoff.attempt()
    }

    /// Start the first attempt immediately. Node intentionally does not call
    /// `backoff.next()` before the first socket attempt.
    pub fn start(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.stopped || self.phase == ConnectionPhase::Closed {
            self.phase = ConnectionPhase::Closed;
            return Ok(LifecycleAction::Exit(LinkExitKind::Stopped));
        }
        if self.phase != ConnectionPhase::Idle {
            return Err(LifecycleError::invalid(self.phase, "start"));
        }
        self.phase = ConnectionPhase::Connecting;
        Ok(LifecycleAction::ConnectNow)
    }

    pub fn socket_opened(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Connecting {
            return Err(LifecycleError::invalid(self.phase, "socket_opened"));
        }
        self.phase = ConnectionPhase::Handshake;
        Ok(LifecycleAction::None)
    }

    /// A socket that closes before the open event is a retryable dropped
    /// attempt in the Node implementation.
    pub fn socket_failed_before_open(
        &mut self,
        now_ms: f64,
        rng_sample: f64,
    ) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Connecting {
            return Err(LifecycleError::invalid(
                self.phase,
                "socket_failed_before_open",
            ));
        }
        Ok(self.retry_after_drop(now_ms, rng_sample))
    }

    pub fn hello_ack_succeeded(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Handshake {
            return Err(LifecycleError::invalid(self.phase, "hello_ack_succeeded"));
        }
        self.backoff.reset();
        self.reconnect_attempt = 0;
        self.reconnect_at_ms = None;
        self.phase = ConnectionPhase::Online;
        Ok(LifecycleAction::Online)
    }

    pub fn hello_ack_refused(
        &mut self,
        code: Option<&str>,
        now_ms: f64,
        rng_sample: f64,
    ) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Handshake {
            return Err(LifecycleError::invalid(self.phase, "hello_ack_refused"));
        }
        Ok(self.apply_directive(classify_hello_ack_refusal(code), now_ms, rng_sample))
    }

    /// A socket that drops after opening but before hello_ack is the same
    /// retryable dropped attempt as the Node `#attemptConnect()` path.
    pub fn socket_failed_during_handshake(
        &mut self,
        now_ms: f64,
        rng_sample: f64,
    ) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Handshake {
            return Err(LifecycleError::invalid(
                self.phase,
                "socket_failed_during_handshake",
            ));
        }
        Ok(self.retry_after_drop(now_ms, rng_sample))
    }

    pub fn handshake_timed_out(
        &mut self,
        now_ms: f64,
        rng_sample: f64,
    ) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Handshake {
            return Err(LifecycleError::invalid(self.phase, "handshake_timed_out"));
        }
        Ok(self.apply_directive(classify_handshake_timeout(), now_ms, rng_sample))
    }

    /// Policy for a socket close observed after the link reached Online.
    pub fn online_socket_closed(
        &mut self,
        code: u16,
        now_ms: f64,
        rng_sample: f64,
    ) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Online {
            return Err(LifecycleError::invalid(self.phase, "online_socket_closed"));
        }
        Ok(self.apply_directive(classify_socket_close(code), now_ms, rng_sample))
    }

    /// Called when the scheduled reconnect delay completes. Reconnect status
    /// metadata intentionally remains populated until a successful hello_ack,
    /// matching Node `getStatus()` while the next attempt is connecting.
    pub fn reconnect_wait_elapsed(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Reconnecting {
            return Err(LifecycleError::invalid(
                self.phase,
                "reconnect_wait_elapsed",
            ));
        }
        if self.stopped {
            self.phase = ConnectionPhase::Closed;
            return Ok(LifecycleAction::Exit(LinkExitKind::Stopped));
        }
        self.phase = ConnectionPhase::Connecting;
        Ok(LifecycleAction::ConnectNow)
    }

    /// Graceful close is represented as a pure caller action. The transport
    /// decides whether it actually has an open socket to drain/close.
    pub fn request_close(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.phase == ConnectionPhase::Closed {
            self.stopped = true;
            return Ok(LifecycleAction::Exit(LinkExitKind::Stopped));
        }
        if self.phase == ConnectionPhase::Closing {
            return Err(LifecycleError::invalid(self.phase, "request_close"));
        }

        let previous = self.phase;
        self.stopped = true;
        self.phase = ConnectionPhase::Closing;
        Ok(match previous {
            ConnectionPhase::Reconnecting => LifecycleAction::CancelReconnectWait,
            ConnectionPhase::Connecting | ConnectionPhase::Handshake | ConnectionPhase::Online => {
                LifecycleAction::CloseSocket
            }
            ConnectionPhase::Idle => LifecycleAction::None,
            ConnectionPhase::Closing | ConnectionPhase::Closed => unreachable!(),
        })
    }

    pub fn finish_close(&mut self) -> Result<LifecycleAction, LifecycleError> {
        if self.phase != ConnectionPhase::Closing {
            return Err(LifecycleError::invalid(self.phase, "finish_close"));
        }
        self.phase = ConnectionPhase::Closed;
        self.reconnect_at_ms = None;
        Ok(LifecycleAction::Exit(LinkExitKind::Stopped))
    }

    fn apply_directive(
        &mut self,
        directive: LinkDirective,
        now_ms: f64,
        rng_sample: f64,
    ) -> LifecycleAction {
        match directive {
            LinkDirective::Retry => self.retry_after_drop(now_ms, rng_sample),
            LinkDirective::Exit(kind) => {
                self.phase = ConnectionPhase::Closed;
                self.reconnect_at_ms = None;
                LifecycleAction::Exit(kind)
            }
        }
    }

    fn retry_after_drop(&mut self, now_ms: f64, rng_sample: f64) -> LifecycleAction {
        if self.stopped {
            self.phase = ConnectionPhase::Closed;
            self.reconnect_at_ms = None;
            return LifecycleAction::Exit(LinkExitKind::Stopped);
        }

        let delay_ms = self.backoff.next(rng_sample);
        let planned_attempt = self.backoff.attempt();
        if self
            .max_reconnect_attempts
            .is_some_and(|max| planned_attempt > max)
        {
            self.phase = ConnectionPhase::Closed;
            self.reconnect_at_ms = None;
            return LifecycleAction::Exit(LinkExitKind::MaxReconnect);
        }

        let reconnect_at_ms = now_ms + delay_ms;
        self.phase = ConnectionPhase::Reconnecting;
        self.reconnect_attempt = planned_attempt;
        self.reconnect_at_ms = Some(reconnect_at_ms);
        LifecycleAction::ScheduleReconnect(ReconnectSchedule {
            attempt: planned_attempt,
            delay_ms,
            reconnect_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{ConnectionPhase, LifecycleAction, LinkLifecycle, ReconnectSchedule};
    use crate::link::backoff::{BackoffOptions, ExponentialBackoff};
    use crate::link::policy::LinkExitKind;

    fn deterministic_backoff() -> ExponentialBackoff {
        ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(100.0),
            max_ms: Some(1_000.0),
            factor: Some(2.0),
            jitter: Some(0.0),
        })
    }

    #[test]
    fn first_attempt_is_immediate_and_success_resets_reconnect_state() {
        let mut lifecycle = LinkLifecycle::new(None, deterministic_backoff());
        assert_eq!(lifecycle.start().unwrap(), LifecycleAction::ConnectNow);
        assert_eq!(lifecycle.backoff_attempt(), 0);
        assert_eq!(lifecycle.phase(), ConnectionPhase::Connecting);
        lifecycle.socket_opened().unwrap();
        assert_eq!(lifecycle.phase(), ConnectionPhase::Handshake);
        assert_eq!(
            lifecycle.hello_ack_succeeded().unwrap(),
            LifecycleAction::Online
        );
        assert_eq!(lifecycle.phase(), ConnectionPhase::Online);
        assert_eq!(lifecycle.reconnect_attempt(), 0);
        assert_eq!(lifecycle.reconnect_at_ms(), None);
        assert_eq!(lifecycle.backoff_attempt(), 0);
    }

    #[test]
    fn ordinary_drop_schedules_then_successful_handshake_resets_backoff() {
        let mut lifecycle = LinkLifecycle::new(None, deterministic_backoff());
        lifecycle.start().unwrap();
        lifecycle.socket_opened().unwrap();
        lifecycle.hello_ack_succeeded().unwrap();

        assert_eq!(
            lifecycle.online_socket_closed(1006, 1_000.0, 0.5).unwrap(),
            LifecycleAction::ScheduleReconnect(ReconnectSchedule {
                attempt: 1,
                delay_ms: 100.0,
                reconnect_at_ms: 1_100.0,
            })
        );
        assert_eq!(lifecycle.reconnect_attempt(), 1);
        assert_eq!(lifecycle.backoff_attempt(), 1);
        lifecycle.reconnect_wait_elapsed().unwrap();
        lifecycle.socket_opened().unwrap();
        lifecycle.hello_ack_succeeded().unwrap();
        assert_eq!(lifecycle.reconnect_attempt(), 0);
        assert_eq!(lifecycle.reconnect_at_ms(), None);
        assert_eq!(lifecycle.backoff_attempt(), 0);
    }

    #[test]
    fn fatal_refusals_and_close_codes_stop_without_reconnect() {
        for (code, expected) in [
            ("auth_expired", LinkExitKind::AuthRejected),
            ("protocol_incompatible", LinkExitKind::ContractRejected),
        ] {
            let mut lifecycle = LinkLifecycle::new(None, deterministic_backoff());
            lifecycle.start().unwrap();
            lifecycle.socket_opened().unwrap();
            assert_eq!(
                lifecycle.hello_ack_refused(Some(code), 0.0, 0.5).unwrap(),
                LifecycleAction::Exit(expected)
            );
            assert_eq!(lifecycle.phase(), ConnectionPhase::Closed);
            assert_eq!(lifecycle.backoff_attempt(), 0);
        }

        for (code, expected) in [
            (4409, LinkExitKind::Superseded),
            (4401, LinkExitKind::AuthRejected),
        ] {
            let mut lifecycle = LinkLifecycle::new(None, deterministic_backoff());
            lifecycle.start().unwrap();
            lifecycle.socket_opened().unwrap();
            lifecycle.hello_ack_succeeded().unwrap();
            assert_eq!(
                lifecycle.online_socket_closed(code, 0.0, 0.5).unwrap(),
                LifecycleAction::Exit(expected)
            );
            assert_eq!(lifecycle.phase(), ConnectionPhase::Closed);
        }
    }

    #[test]
    fn handshake_timeout_and_unknown_refusal_are_retryable() {
        let mut timeout = LinkLifecycle::new(None, deterministic_backoff());
        timeout.start().unwrap();
        timeout.socket_opened().unwrap();
        assert!(matches!(
            timeout.handshake_timed_out(10.0, 0.5).unwrap(),
            LifecycleAction::ScheduleReconnect(_)
        ));

        let mut unknown = LinkLifecycle::new(None, deterministic_backoff());
        unknown.start().unwrap();
        unknown.socket_opened().unwrap();
        assert!(matches!(
            unknown
                .hello_ack_refused(Some("edge_hiccup"), 10.0, 0.5)
                .unwrap(),
            LifecycleAction::ScheduleReconnect(_)
        ));
    }

    #[test]
    fn close_during_reconnect_cancels_wait_then_finishes_closed() {
        let mut lifecycle = LinkLifecycle::new(None, deterministic_backoff());
        lifecycle.start().unwrap();
        assert!(matches!(
            lifecycle.socket_failed_before_open(10.0, 0.5).unwrap(),
            LifecycleAction::ScheduleReconnect(_)
        ));
        assert_eq!(lifecycle.phase(), ConnectionPhase::Reconnecting);
        assert_eq!(
            lifecycle.request_close().unwrap(),
            LifecycleAction::CancelReconnectWait
        );
        assert!(lifecycle.stopped());
        assert_eq!(lifecycle.phase(), ConnectionPhase::Closing);
        assert_eq!(
            lifecycle.finish_close().unwrap(),
            LifecycleAction::Exit(LinkExitKind::Stopped)
        );
        assert_eq!(lifecycle.phase(), ConnectionPhase::Closed);
    }

    #[test]
    fn shared_batch5_fixture_matches_lifecycle_oracle() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/link-lifecycle-policy-batch5.json"
        ))
        .expect("shared link lifecycle fixture");

        let expected_phases = fixture["connection_phases"]["set"]
            .as_array()
            .expect("phase set")
            .iter()
            .map(|value| value.as_str().expect("phase string"))
            .collect::<Vec<_>>();
        assert_eq!(
            ConnectionPhase::ALL
                .iter()
                .map(|phase| phase.as_str())
                .collect::<Vec<_>>(),
            expected_phases
        );

        for case in fixture["reconnect_cap"].as_array().expect("reconnect_cap") {
            assert_eq!(case["oracle"].as_str(), Some("node_parity"));
            let max = case["maxReconnectAttempts"].as_u64();
            let expected_exit = case["expected_exit"].as_str();
            let observe_attempts = case
                .get("observe_attempts")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let expected_factory_calls = case.get("factory_calls").and_then(Value::as_u64);

            let mut lifecycle = LinkLifecycle::new(
                max,
                ExponentialBackoff::new(BackoffOptions {
                    base_ms: Some(1.0),
                    max_ms: Some(1.0),
                    factor: Some(2.0),
                    jitter: Some(0.0),
                }),
            );
            let mut factory_calls = 1_u64;
            assert_eq!(lifecycle.start().unwrap(), LifecycleAction::ConnectNow);

            loop {
                let action = lifecycle
                    .socket_failed_before_open(factory_calls as f64, 0.0)
                    .unwrap();
                match action {
                    LifecycleAction::ScheduleReconnect(schedule) => {
                        if max.is_none() {
                            assert!(
                                observe_attempts > 0,
                                "unlimited fixture needs observe_attempts"
                            );
                        }
                        if max.is_none() && schedule.attempt >= observe_attempts {
                            assert_eq!(expected_exit, None);
                            break;
                        }
                        assert_eq!(
                            lifecycle.reconnect_wait_elapsed().unwrap(),
                            LifecycleAction::ConnectNow
                        );
                        factory_calls += 1;
                    }
                    LifecycleAction::Exit(kind) => {
                        assert_eq!(Some(kind.as_str()), expected_exit);
                        assert_eq!(Some(factory_calls), expected_factory_calls);
                        break;
                    }
                    other => panic!("unexpected reconnect action: {other:?}"),
                }
            }
        }
    }
}
