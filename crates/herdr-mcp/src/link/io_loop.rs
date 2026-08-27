//! Async composition loop for the staged Rust workstation Link.
//!
//! This layer owns only I/O orchestration: cancellable socket connection
//! attempts, timer delivery, action routing, and graceful stop/drain ordering.
//! Protocol, reconnect, close-code, heartbeat/silence, request settlement, and
//! generation-fencing policy remain in the lower staged kernels.
//!
//! No CLI, daemon, service, runtime-current, or production path constructs this
//! loop yet. Production Link remains on the Node implementation until later
//! cutover gates are complete.

use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::lifecycle::{ConnectionPhase, ReconnectSchedule};
use super::local_mcp::LinkRuntimeTransport;
use super::policy::LinkExitKind;
use super::runner::{LinkRunnerCore, RunnerError};
use super::socket_driver::{
    ABNORMAL_CLOSE_CODE, LINK_SUBPROTOCOL, SocketAttemptHandle, SocketDriverConfig,
    SocketDriverError, WebSocketEvent, connect_socket_attempt, feed_socket_event,
};
use super::transport::{LinkTransportCore, SocketAttemptId, TransportAction, TransportError};
use crate::relay::protocol::RelayMessage;

pub(crate) const LINK_DEFAULT_DRAIN_MS: u64 = 5_000;

#[derive(Clone)]
pub(crate) struct LinkIoConfig {
    pub edge_url: String,
    pub application_protocol: String,
    pub link_token: String,
    pub socket: SocketDriverConfig,
    pub drain_ms: u64,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub rng_sample: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl LinkIoConfig {
    pub(crate) fn new(edge_url: impl Into<String>, link_token: impl Into<String>) -> Self {
        Self {
            edge_url: edge_url.into(),
            application_protocol: LINK_SUBPROTOCOL.to_owned(),
            link_token: link_token.into(),
            socket: SocketDriverConfig::default(),
            drain_ms: LINK_DEFAULT_DRAIN_MS,
            now_ms: Arc::new(system_now_ms),
            rng_sample: Arc::new(system_rng_sample),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkIoError {
    Socket(SocketDriverError),
    Transport(TransportError),
    Runner(RunnerError),
    InternalEventChannelClosed,
}

impl From<SocketDriverError> for LinkIoError {
    fn from(value: SocketDriverError) -> Self {
        Self::Socket(value)
    }
}

impl From<TransportError> for LinkIoError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<RunnerError> for LinkIoError {
    fn from(value: RunnerError) -> Self {
        Self::Runner(value)
    }
}

pub(crate) struct SocketConnectRequest {
    edge_url: String,
    application_protocol: String,
    link_token: String,
    attempt_id: SocketAttemptId,
    config: SocketDriverConfig,
}

pub(crate) trait LoopSocketHandle: Send + 'static {
    fn attempt_id(&self) -> SocketAttemptId;
    fn next_event(&mut self) -> impl Future<Output = Option<WebSocketEvent>> + Send;
    fn execute_action(
        &self,
        action: &TransportAction,
    ) -> impl Future<Output = Result<bool, SocketDriverError>> + Send;
    fn abort(&self);
}

impl LoopSocketHandle for SocketAttemptHandle {
    fn attempt_id(&self) -> SocketAttemptId {
        SocketAttemptHandle::attempt_id(self)
    }

    fn next_event(&mut self) -> impl Future<Output = Option<WebSocketEvent>> + Send {
        SocketAttemptHandle::next_event(self)
    }

    fn execute_action(
        &self,
        action: &TransportAction,
    ) -> impl Future<Output = Result<bool, SocketDriverError>> + Send {
        SocketAttemptHandle::execute_action(self, action)
    }

    fn abort(&self) {
        SocketAttemptHandle::abort(self);
    }
}

pub(crate) trait LoopSocketConnector: Send + Sync + 'static {
    type Handle: LoopSocketHandle;

    fn connect(
        &self,
        request: SocketConnectRequest,
    ) -> impl Future<Output = Result<Self::Handle, SocketDriverError>> + Send;
}

#[derive(Debug, Default)]
pub(crate) struct ProductionSocketConnector;

impl LoopSocketConnector for ProductionSocketConnector {
    type Handle = SocketAttemptHandle;

    async fn connect(
        &self,
        request: SocketConnectRequest,
    ) -> Result<Self::Handle, SocketDriverError> {
        connect_socket_attempt(
            &request.edge_url,
            &request.application_protocol,
            &request.link_token,
            request.attempt_id,
            request.config,
        )
        .await
    }
}

enum LoopEvent<H> {
    ConnectCompleted {
        attempt_id: SocketAttemptId,
        result: Result<H, SocketDriverError>,
    },
    HandshakeTimeout {
        attempt_id: SocketAttemptId,
    },
    ReconnectElapsed {
        generation: u64,
        expected_attempt: u64,
    },
    HeartbeatTick {
        generation: u64,
    },
    SilenceTick {
        generation: u64,
    },
    DrainElapsed {
        generation: u64,
    },
}

enum SocketPoll {
    Event(WebSocketEvent),
    ChannelClosed(SocketAttemptId),
}

pub(crate) struct LinkIoLoop<T, C = ProductionSocketConnector>
where
    T: LinkRuntimeTransport,
    C: LoopSocketConnector,
{
    config: LinkIoConfig,
    core: LinkTransportCore,
    runner: LinkRunnerCore<T>,
    connector: Arc<C>,
    socket: Option<C::Handle>,
    connect_attempt: Option<SocketAttemptId>,
    connect_task: Option<JoinHandle<()>>,
    event_tx: mpsc::UnboundedSender<LoopEvent<C::Handle>>,
    event_rx: mpsc::UnboundedReceiver<LoopEvent<C::Handle>>,
    handshake_attempt: Option<SocketAttemptId>,
    handshake_timer: Option<JoinHandle<()>>,
    reconnect_generation: u64,
    reconnect_timer: Option<JoinHandle<()>>,
    online_generation: u64,
    online_delays: Option<(Duration, Duration)>,
    heartbeat_timer: Option<JoinHandle<()>>,
    silence_timer: Option<JoinHandle<()>>,
    drain_generation: u64,
    drain_timer: Option<JoinHandle<()>>,
    stopping: bool,
    draining: bool,
}

impl<T> LinkIoLoop<T, ProductionSocketConnector>
where
    T: LinkRuntimeTransport,
{
    pub(crate) fn production(
        config: LinkIoConfig,
        core: LinkTransportCore,
        runner: LinkRunnerCore<T>,
    ) -> Self {
        Self::with_connector(config, core, runner, Arc::new(ProductionSocketConnector))
    }
}

impl<T, C> LinkIoLoop<T, C>
where
    T: LinkRuntimeTransport,
    C: LoopSocketConnector,
{
    pub(crate) fn with_connector(
        config: LinkIoConfig,
        core: LinkTransportCore,
        runner: LinkRunnerCore<T>,
        connector: Arc<C>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            config,
            core,
            runner,
            connector,
            socket: None,
            connect_attempt: None,
            connect_task: None,
            event_tx,
            event_rx,
            handshake_attempt: None,
            handshake_timer: None,
            reconnect_generation: 0,
            reconnect_timer: None,
            online_generation: 0,
            online_delays: None,
            heartbeat_timer: None,
            silence_timer: None,
            drain_generation: 0,
            drain_timer: None,
            stopping: false,
            draining: false,
        }
    }

    pub(crate) async fn run(
        mut self,
        mut stop_rx: watch::Receiver<bool>,
    ) -> Result<LinkExitKind, LinkIoError> {
        let start = self.core.start()?;
        if let Some(exit) = self.pump_actions(start).await? {
            return self.normalize_exit(exit);
        }

        loop {
            if self.draining
                && self.runner.active_requests() == 0
                && let Some(exit) = self.finish_stop().await?
            {
                return self.normalize_exit(exit);
            }

            if !self.stopping && *stop_rx.borrow() {
                if let Some(exit) = self.start_stop().await? {
                    return self.normalize_exit(exit);
                }
                continue;
            }

            let settlement_clock = Arc::clone(&self.config.now_ms);
            tokio::select! {
                changed = stop_rx.changed(), if !self.stopping => {
                    if (changed.is_err() || *stop_rx.borrow())
                        && let Some(exit) = self.start_stop().await?
                    {
                        return self.normalize_exit(exit);
                    }
                }
                socket_poll = next_socket_poll(&mut self.socket) => {
                    if let Some(exit) = self.handle_socket_poll(socket_poll).await? {
                        return self.normalize_exit(exit);
                    }
                }
                internal = self.event_rx.recv() => {
                    let internal = internal.ok_or(LinkIoError::InternalEventChannelClosed)?;
                    if let Some(exit) = self.handle_internal_event(internal).await? {
                        return self.normalize_exit(exit);
                    }
                }
                runtime = self.runner.next_runtime_actions_with_now(
                    &self.core,
                    move || settlement_clock(),
                ) => {
                    let actions = runtime?;
                    if let Some(exit) = self.pump_actions(actions).await? {
                        return self.normalize_exit(exit);
                    }
                }
            }
        }
    }

    fn now_ms(&self) -> i64 {
        (self.config.now_ms)()
    }

    fn rng_sample(&self) -> f64 {
        (self.config.rng_sample)()
    }

    async fn handle_socket_poll(
        &mut self,
        socket_poll: SocketPoll,
    ) -> Result<Option<LinkExitKind>, LinkIoError> {
        let (event, closed_attempt) = match socket_poll {
            SocketPoll::Event(event) => {
                let closed_attempt = match &event {
                    WebSocketEvent::Closed { attempt_id, .. } => Some(*attempt_id),
                    _ => None,
                };
                (event, closed_attempt)
            }
            SocketPoll::ChannelClosed(attempt_id) => (
                WebSocketEvent::Closed {
                    attempt_id,
                    code: ABNORMAL_CLOSE_CODE,
                    reason: "socket event channel closed".to_owned(),
                },
                Some(attempt_id),
            ),
        };

        if let Some(attempt_id) = closed_attempt
            && self.socket.as_ref().map(LoopSocketHandle::attempt_id) == Some(attempt_id)
        {
            self.socket.take();
        }

        let now_ms = self.now_ms();
        let hello = if matches!(event, WebSocketEvent::Opened { .. }) {
            match self.runner.hello_message(now_ms) {
                RelayMessage::Hello(hello) => Some(hello),
                _ => unreachable!("hello_message always returns hello"),
            }
        } else {
            None
        };
        let rng_sample = self.rng_sample();
        let actions = feed_socket_event(&mut self.core, event, hello, now_ms, rng_sample)?;
        self.pump_actions(actions).await
    }

    async fn handle_internal_event(
        &mut self,
        event: LoopEvent<C::Handle>,
    ) -> Result<Option<LinkExitKind>, LinkIoError> {
        match event {
            LoopEvent::ConnectCompleted { attempt_id, result } => {
                if self.connect_attempt != Some(attempt_id) {
                    if let Ok(handle) = result {
                        handle.abort();
                    }
                    return Ok(None);
                }
                self.connect_task.take();
                self.connect_attempt = None;
                if self.core.active_attempt() != Some(attempt_id) {
                    if let Ok(handle) = result {
                        handle.abort();
                    }
                    return Ok(None);
                }
                match result {
                    Ok(handle) => {
                        self.socket = Some(handle);
                        Ok(None)
                    }
                    Err(_) => {
                        let actions = self.core.socket_connect_failed(
                            attempt_id,
                            self.now_ms(),
                            self.rng_sample(),
                        )?;
                        self.pump_actions(actions).await
                    }
                }
            }
            LoopEvent::HandshakeTimeout { attempt_id } => {
                if self.handshake_attempt != Some(attempt_id) {
                    return Ok(None);
                }
                self.handshake_attempt = None;
                self.handshake_timer.take();
                let actions =
                    self.core
                        .handshake_timed_out(attempt_id, self.now_ms(), self.rng_sample())?;
                self.pump_actions(actions).await
            }
            LoopEvent::ReconnectElapsed {
                generation,
                expected_attempt,
            } => {
                if generation != self.reconnect_generation {
                    return Ok(None);
                }
                self.reconnect_timer.take();
                let actions = self.core.reconnect_wait_elapsed(expected_attempt)?;
                self.pump_actions(actions).await
            }
            LoopEvent::HeartbeatTick { generation } => {
                if generation != self.online_generation || self.online_delays.is_none() {
                    return Ok(None);
                }
                self.heartbeat_timer.take();
                let actions = self.core.heartbeat_tick();
                let exit = self.pump_actions(actions).await?;
                if exit.is_none()
                    && generation == self.online_generation
                    && let Some((heartbeat_delay, _)) = self.online_delays
                {
                    self.arm_heartbeat(generation, heartbeat_delay);
                }
                Ok(exit)
            }
            LoopEvent::SilenceTick { generation } => {
                if generation != self.online_generation || self.online_delays.is_none() {
                    return Ok(None);
                }
                self.silence_timer.take();
                let actions = self.core.silence_tick(self.now_ms());
                let exit = self.pump_actions(actions).await?;
                if exit.is_none()
                    && generation == self.online_generation
                    && let Some((_, silence_delay)) = self.online_delays
                {
                    self.arm_silence(generation, silence_delay);
                }
                Ok(exit)
            }
            LoopEvent::DrainElapsed { generation } => {
                if generation != self.drain_generation || !self.draining {
                    return Ok(None);
                }
                self.drain_timer.take();
                self.draining = false;
                self.finish_stop().await
            }
        }
    }

    async fn start_stop(&mut self) -> Result<Option<LinkExitKind>, LinkIoError> {
        if self.stopping {
            return Ok(None);
        }
        self.stopping = true;
        self.runner.mark_stopping();
        self.stop_online_timers();

        let can_drain = self.core.phase() == ConnectionPhase::Online
            && self.socket.is_some()
            && self.runner.active_requests() > 0
            && self.config.drain_ms > 0;
        if can_drain {
            self.draining = true;
            self.arm_drain(Duration::from_millis(self.config.drain_ms));
            return Ok(None);
        }
        self.finish_stop().await
    }

    async fn finish_stop(&mut self) -> Result<Option<LinkExitKind>, LinkIoError> {
        self.draining = false;
        self.cancel_drain();
        self.stop_online_timers();

        let now_ms = self.now_ms();
        let mut actions = self.core.request_close()?;
        // Match Node close ordering: initiate close first, then settle any
        // leftover request locally. Any resulting frame send is best-effort.
        let leftovers = self.runner.reject_pending(now_ms)?;
        for message in leftovers {
            actions.extend(self.core.send_outbound(message)?);
        }
        self.pump_actions(actions).await
    }

    fn normalize_exit(&mut self, exit: LinkExitKind) -> Result<LinkExitKind, LinkIoError> {
        if !self.stopping {
            return Ok(exit);
        }
        // Node suppresses a concurrent socket/fatal outcome once close() has
        // marked the link stopped. Ensure local request/fence state is also
        // released before returning that graceful outcome.
        let _ = self.runner.reject_pending(self.now_ms())?;
        Ok(LinkExitKind::Stopped)
    }

    async fn pump_actions(
        &mut self,
        actions: Vec<TransportAction>,
    ) -> Result<Option<LinkExitKind>, LinkIoError> {
        let routed = self
            .runner
            .route_transport_actions(&self.core, actions, self.now_ms())
            .await?;
        let mut queue = VecDeque::from(routed);
        let mut exit = None;

        while let Some(action) = queue.pop_front() {
            match action {
                TransportAction::OpenSocket { attempt_id } => {
                    self.spawn_connect(attempt_id);
                }
                action @ (TransportAction::SendFrame { .. }
                | TransportAction::CloseSocket { .. }
                | TransportAction::TerminateSocket { .. }) => {
                    let generated = self.execute_socket_action(&action).await?;
                    queue.extend(generated);
                }
                TransportAction::ArmHandshakeTimeout {
                    attempt_id,
                    delay_ms,
                } => self.arm_handshake(attempt_id, duration_from_i64_ms(delay_ms)),
                TransportAction::CancelHandshakeTimeout { attempt_id } => {
                    self.cancel_handshake(attempt_id);
                }
                TransportAction::ScheduleReconnect(schedule) => {
                    if !self.stopping {
                        self.arm_reconnect(schedule);
                    }
                }
                TransportAction::CancelReconnectWait => self.cancel_reconnect(),
                TransportAction::StartOnlineTimers {
                    heartbeat_ms,
                    silence_check_ms,
                } => self.start_online_timers(heartbeat_ms, silence_check_ms),
                TransportAction::StopOnlineTimers => self.stop_online_timers(),
                TransportAction::OversizedInbound {
                    request_id: Some(request_id),
                    size_bytes,
                    max_bytes,
                    ..
                } => {
                    let message = self.runner.oversized_inbound_error(
                        request_id,
                        size_bytes,
                        max_bytes,
                        self.now_ms(),
                    );
                    queue.extend(self.core.send_outbound(message)?);
                }
                TransportAction::OversizedInbound {
                    request_id: None, ..
                }
                | TransportAction::Online { .. }
                | TransportAction::Disconnected { .. }
                | TransportAction::InboundRejected { .. }
                | TransportAction::SocketErrorObserved { .. } => {}
                TransportAction::HeartbeatDue { .. } | TransportAction::Inbound { .. } => {
                    unreachable!("runner routes higher-layer transport actions before the I/O pump")
                }
                TransportAction::Exit(kind) => {
                    if exit.is_none() {
                        exit = Some(kind);
                    }
                }
            }
        }
        Ok(exit)
    }

    fn spawn_connect(&mut self, attempt_id: SocketAttemptId) {
        self.abort_connect();
        if let Some(socket) = self.socket.take() {
            socket.abort();
        }
        let request = SocketConnectRequest {
            edge_url: self.config.edge_url.clone(),
            application_protocol: self.config.application_protocol.clone(),
            link_token: self.config.link_token.clone(),
            attempt_id,
            config: self.config.socket,
        };
        let connector = Arc::clone(&self.connector);
        let event_tx = self.event_tx.clone();
        self.connect_attempt = Some(attempt_id);
        self.connect_task = Some(tokio::spawn(async move {
            let result = connector.connect(request).await;
            let _ = event_tx.send(LoopEvent::ConnectCompleted { attempt_id, result });
        }));
    }

    async fn execute_socket_action(
        &mut self,
        action: &TransportAction,
    ) -> Result<Vec<TransportAction>, LinkIoError> {
        let Some(attempt_id) = socket_action_attempt(action) else {
            return Ok(Vec::new());
        };

        if self.socket.as_ref().map(LoopSocketHandle::attempt_id) == Some(attempt_id) {
            let result = self
                .socket
                .as_ref()
                .expect("socket id checked")
                .execute_action(action)
                .await;
            match result {
                Ok(_) => return Ok(Vec::new()),
                Err(_) => {
                    if let Some(socket) = self.socket.take() {
                        socket.abort();
                    }
                    return Ok(self.core.socket_closed(
                        attempt_id,
                        ABNORMAL_CLOSE_CODE,
                        "socket command channel failed",
                        self.now_ms(),
                        self.rng_sample(),
                    )?);
                }
            }
        }

        if self.connect_attempt == Some(attempt_id)
            && matches!(
                action,
                TransportAction::CloseSocket { .. } | TransportAction::TerminateSocket { .. }
            )
        {
            self.abort_connect();
            return Ok(self.core.socket_closed(
                attempt_id,
                ABNORMAL_CLOSE_CODE,
                "connect attempt aborted",
                self.now_ms(),
                self.rng_sample(),
            )?);
        }
        Ok(Vec::new())
    }

    fn arm_handshake(&mut self, attempt_id: SocketAttemptId, delay: Duration) {
        abort_task(&mut self.handshake_timer);
        self.handshake_attempt = Some(attempt_id);
        let event_tx = self.event_tx.clone();
        self.handshake_timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(LoopEvent::HandshakeTimeout { attempt_id });
        }));
    }

    fn cancel_handshake(&mut self, attempt_id: SocketAttemptId) {
        if self.handshake_attempt == Some(attempt_id) {
            self.handshake_attempt = None;
            abort_task(&mut self.handshake_timer);
        }
    }

    fn arm_reconnect(&mut self, schedule: ReconnectSchedule) {
        self.cancel_reconnect();
        self.reconnect_generation = self.reconnect_generation.saturating_add(1);
        let generation = self.reconnect_generation;
        let expected_attempt = schedule.attempt;
        let event_tx = self.event_tx.clone();
        let delay = duration_from_f64_ms(schedule.delay_ms);
        self.reconnect_timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(LoopEvent::ReconnectElapsed {
                generation,
                expected_attempt,
            });
        }));
    }

    fn cancel_reconnect(&mut self) {
        self.reconnect_generation = self.reconnect_generation.saturating_add(1);
        abort_task(&mut self.reconnect_timer);
    }

    fn start_online_timers(&mut self, heartbeat_ms: i64, silence_check_ms: f64) {
        self.stop_online_timers();
        self.online_generation = self.online_generation.saturating_add(1);
        let generation = self.online_generation;
        let heartbeat_delay = duration_from_i64_ms(heartbeat_ms);
        let silence_delay = duration_from_f64_ms(silence_check_ms);
        self.online_delays = Some((heartbeat_delay, silence_delay));
        self.arm_heartbeat(generation, heartbeat_delay);
        self.arm_silence(generation, silence_delay);
    }

    fn stop_online_timers(&mut self) {
        self.online_generation = self.online_generation.saturating_add(1);
        self.online_delays = None;
        abort_task(&mut self.heartbeat_timer);
        abort_task(&mut self.silence_timer);
    }

    fn arm_heartbeat(&mut self, generation: u64, delay: Duration) {
        abort_task(&mut self.heartbeat_timer);
        let event_tx = self.event_tx.clone();
        self.heartbeat_timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(LoopEvent::HeartbeatTick { generation });
        }));
    }

    fn arm_silence(&mut self, generation: u64, delay: Duration) {
        abort_task(&mut self.silence_timer);
        let event_tx = self.event_tx.clone();
        self.silence_timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(LoopEvent::SilenceTick { generation });
        }));
    }

    fn arm_drain(&mut self, delay: Duration) {
        self.cancel_drain();
        self.drain_generation = self.drain_generation.saturating_add(1);
        let generation = self.drain_generation;
        let event_tx = self.event_tx.clone();
        self.drain_timer = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(LoopEvent::DrainElapsed { generation });
        }));
    }

    fn cancel_drain(&mut self) {
        self.drain_generation = self.drain_generation.saturating_add(1);
        abort_task(&mut self.drain_timer);
    }

    fn abort_connect(&mut self) {
        self.connect_attempt = None;
        abort_task(&mut self.connect_task);
    }
}

impl<T, C> Drop for LinkIoLoop<T, C>
where
    T: LinkRuntimeTransport,
    C: LoopSocketConnector,
{
    fn drop(&mut self) {
        self.abort_connect();
        if let Some(socket) = self.socket.take() {
            socket.abort();
        }
        abort_task(&mut self.handshake_timer);
        abort_task(&mut self.reconnect_timer);
        abort_task(&mut self.heartbeat_timer);
        abort_task(&mut self.silence_timer);
        abort_task(&mut self.drain_timer);
    }
}

async fn next_socket_poll<H: LoopSocketHandle>(socket: &mut Option<H>) -> SocketPoll {
    let Some(socket) = socket.as_mut() else {
        return pending::<SocketPoll>().await;
    };
    let attempt_id = socket.attempt_id();
    match socket.next_event().await {
        Some(event) => SocketPoll::Event(event),
        None => SocketPoll::ChannelClosed(attempt_id),
    }
}

fn socket_action_attempt(action: &TransportAction) -> Option<SocketAttemptId> {
    match action {
        TransportAction::SendFrame { attempt_id, .. }
        | TransportAction::CloseSocket { attempt_id, .. }
        | TransportAction::TerminateSocket { attempt_id } => Some(*attempt_id),
        _ => None,
    }
}

fn abort_task(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

fn duration_from_i64_ms(value: i64) -> Duration {
    Duration::from_millis(value.max(0) as u64)
}

fn duration_from_f64_ms(value: f64) -> Duration {
    let value = if value.is_finite() && value > 0.0 {
        value.ceil()
    } else {
        0.0
    };
    Duration::from_millis(value.min(u64::MAX as f64) as u64)
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn system_rng_sample() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| f64::from(value.subsec_nanos()) / 1_000_000_000.0)
        .unwrap_or(0.5)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Map, Number, json};
    use tokio::sync::{Notify, Semaphore, mpsc, watch};

    use super::{
        LinkIoConfig, LinkIoLoop, LoopSocketConnector, LoopSocketHandle, SocketConnectRequest,
    };
    use crate::link::backoff::{BackoffOptions, ExponentialBackoff};
    use crate::link::local_mcp::{LinkRuntimeTransport, RuntimeHealth, RuntimeToolResult};
    use crate::link::policy::{LinkExitKind, WS_CLOSE_NORMAL, WS_CLOSE_SUPERSEDED};
    use crate::link::request_core::RuntimeRequest;
    use crate::link::runner::{LinkRunnerCore, RunnerConfig};
    use crate::link::socket_driver::{
        LINK_SUBPROTOCOL, SocketDriverError, WebSocketCommand, WebSocketEvent, command_for_action,
    };
    use crate::link::transport::{
        LinkTransportCore, SocketAttemptId, TransportAction, TransportConfig,
    };
    use crate::relay::protocol::{
        DeliveryState, HelloAckMessage, HelloAckOutcome, RelayEnvelope, RelayMessage,
        RuntimeContractInfo, StatusFields, StatusMessage, ToolRequestMessage,
    };
    use crate::relay::wire::{decode_relay_frame, encode_relay_message};

    #[derive(Clone, Copy)]
    enum ConnectPlan {
        Success(Duration),
        Fail(Duration),
    }

    struct FakeSocketControl {
        attempt_id: SocketAttemptId,
        event_tx: mpsc::UnboundedSender<WebSocketEvent>,
        command_rx: mpsc::UnboundedReceiver<WebSocketCommand>,
        aborted: Arc<AtomicBool>,
    }

    struct FakeSocketHandle {
        attempt_id: SocketAttemptId,
        event_rx: mpsc::UnboundedReceiver<WebSocketEvent>,
        command_tx: mpsc::UnboundedSender<WebSocketCommand>,
        aborted: Arc<AtomicBool>,
        refuse_commands: Arc<AtomicBool>,
    }

    impl LoopSocketHandle for FakeSocketHandle {
        fn attempt_id(&self) -> SocketAttemptId {
            self.attempt_id
        }

        async fn next_event(&mut self) -> Option<WebSocketEvent> {
            self.event_rx.recv().await
        }

        async fn execute_action(
            &self,
            action: &TransportAction,
        ) -> Result<bool, SocketDriverError> {
            let Some(command) = command_for_action(action) else {
                return Ok(false);
            };
            if command.attempt_id() != self.attempt_id {
                return Ok(false);
            }
            if self.refuse_commands.load(Ordering::Acquire) {
                return Err(SocketDriverError::CommandChannelFull);
            }
            self.command_tx
                .send(command)
                .map_err(|_| SocketDriverError::CommandChannelClosed)?;
            Ok(true)
        }

        fn abort(&self) {
            self.aborted.store(true, Ordering::Release);
        }
    }

    struct FakeConnector {
        plans: Mutex<VecDeque<ConnectPlan>>,
        attempts: Mutex<Vec<SocketAttemptId>>,
        control_tx: mpsc::UnboundedSender<FakeSocketControl>,
        refuse_commands: Arc<AtomicBool>,
    }

    impl FakeConnector {
        fn new(
            plans: impl IntoIterator<Item = ConnectPlan>,
        ) -> (Arc<Self>, mpsc::UnboundedReceiver<FakeSocketControl>) {
            let (control_tx, control_rx) = mpsc::unbounded_channel();
            (
                Arc::new(Self {
                    plans: Mutex::new(plans.into_iter().collect()),
                    attempts: Mutex::new(Vec::new()),
                    control_tx,
                    refuse_commands: Arc::new(AtomicBool::new(false)),
                }),
                control_rx,
            )
        }

        fn refuse_further_commands(&self) {
            self.refuse_commands.store(true, Ordering::Release);
        }

        fn allow_commands(&self) {
            self.refuse_commands.store(false, Ordering::Release);
        }

        fn attempts(&self) -> Vec<SocketAttemptId> {
            self.attempts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl LoopSocketConnector for FakeConnector {
        type Handle = FakeSocketHandle;

        fn connect(
            &self,
            request: SocketConnectRequest,
        ) -> impl std::future::Future<Output = Result<Self::Handle, SocketDriverError>> + Send
        {
            self.attempts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.attempt_id);
            let plan = self
                .plans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .unwrap_or(ConnectPlan::Success(Duration::ZERO));
            let control_tx = self.control_tx.clone();
            let refuse_commands = Arc::clone(&self.refuse_commands);
            async move {
                let delay = match plan {
                    ConnectPlan::Success(delay) | ConnectPlan::Fail(delay) => delay,
                };
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if matches!(plan, ConnectPlan::Fail(_)) {
                    return Err(SocketDriverError::ConnectFailed);
                }
                let (event_tx, event_rx) = mpsc::unbounded_channel();
                let (command_tx, command_rx) = mpsc::unbounded_channel();
                let aborted = Arc::new(AtomicBool::new(false));
                control_tx
                    .send(FakeSocketControl {
                        attempt_id: request.attempt_id,
                        event_tx,
                        command_rx,
                        aborted: Arc::clone(&aborted),
                    })
                    .map_err(|_| SocketDriverError::CommandChannelClosed)?;
                Ok(FakeSocketHandle {
                    attempt_id: request.attempt_id,
                    event_rx,
                    command_tx,
                    aborted,
                    refuse_commands,
                })
            }
        }
    }

    struct MockRuntime {
        result: RuntimeToolResult,
        gate: Option<Arc<Notify>>,
        started: Option<Arc<Semaphore>>,
        health_gate: Option<Arc<Notify>>,
        health_started: Option<Arc<Notify>>,
    }

    impl MockRuntime {
        fn immediate(result: RuntimeToolResult) -> Self {
            Self {
                result,
                gate: None,
                started: None,
                health_gate: None,
                health_started: None,
            }
        }

        fn blocked(result: RuntimeToolResult, gate: Arc<Notify>, started: Arc<Semaphore>) -> Self {
            Self {
                result,
                gate: Some(gate),
                started: Some(started),
                health_gate: None,
                health_started: None,
            }
        }

        fn blocked_health(health_gate: Arc<Notify>, health_started: Arc<Notify>) -> Self {
            Self {
                result: RuntimeToolResult::Success { result: None },
                gate: None,
                started: None,
                health_gate: Some(health_gate),
                health_started: Some(health_started),
            }
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
                runtime_generation: Some("gen-a".to_owned()),
                contract_epoch: Number::from(2),
                contract_hash: Some(format!("sha256:{}", "a".repeat(64))),
                herdr_version: None,
                herdr_protocol: None,
            }
        }

        async fn dispatch_request(&self, _request: RuntimeRequest) -> RuntimeToolResult {
            if let Some(started) = &self.started {
                started.add_permits(1);
            }
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            self.result.clone()
        }

        async fn cancel_request(&self, _request_id: &str, _reason: &str) {}

        async fn get_health(&self) -> RuntimeHealth {
            if let Some(started) = &self.health_started {
                started.notify_waiters();
            }
            if let Some(gate) = &self.health_gate {
                gate.notified().await;
            }
            RuntimeHealth {
                healthy: true,
                details: None,
            }
        }
    }

    fn make_core(
        heartbeat_ms: i64,
        handshake_timeout_ms: i64,
        max_silence_ms: i64,
        max_frame_bytes: usize,
        max_reconnect_attempts: Option<u64>,
    ) -> LinkTransportCore {
        let backoff = ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(5.0),
            max_ms: Some(5.0),
            factor: Some(1.0),
            jitter: Some(0.0),
        });
        LinkTransportCore::new(
            "ws1",
            max_reconnect_attempts,
            backoff,
            TransportConfig {
                heartbeat_ms,
                handshake_timeout_ms,
                max_frame_bytes,
                max_silence_ms,
            },
        )
    }

    fn make_runner(runtime: Arc<MockRuntime>) -> LinkRunnerCore<MockRuntime> {
        let mut config = RunnerConfig::new("ws1", "boot1", 0);
        config.request_timeout_ms = 5_000;
        config.max_pending = 8;
        LinkRunnerCore::new(config, runtime, "gen-a")
    }

    fn io_config(drain_ms: u64) -> LinkIoConfig {
        let started = Instant::now();
        LinkIoConfig {
            edge_url: "wss://edge.test/link".to_owned(),
            application_protocol: LINK_SUBPROTOCOL.to_owned(),
            link_token: "test-link-token".to_owned(),
            socket: Default::default(),
            drain_ms,
            now_ms: Arc::new(move || {
                10_000_i64.saturating_add(started.elapsed().as_millis() as i64)
            }),
            rng_sample: Arc::new(|| 0.0),
        }
    }

    async fn next_control(
        controls: &mut mpsc::UnboundedReceiver<FakeSocketControl>,
    ) -> FakeSocketControl {
        tokio::time::timeout(Duration::from_secs(1), controls.recv())
            .await
            .expect("connector control timeout")
            .expect("connector control closed")
    }

    async fn next_command(control: &mut FakeSocketControl) -> WebSocketCommand {
        tokio::time::timeout(Duration::from_secs(1), control.command_rx.recv())
            .await
            .expect("socket command timeout")
            .expect("socket command channel closed")
    }

    fn send_open(control: &FakeSocketControl) {
        control
            .event_tx
            .send(WebSocketEvent::Opened {
                attempt_id: control.attempt_id,
                selected_protocol: LINK_SUBPROTOCOL.to_owned(),
            })
            .expect("opened event accepted");
    }

    fn hello_ack_success() -> String {
        encode_relay_message(
            &RelayMessage::HelloAck(HelloAckMessage {
                envelope: RelayEnvelope::new("ws1"),
                outcome: HelloAckOutcome::Success {
                    server_version: Some("edge-test".to_owned()),
                    edge_deployment_id: None,
                    capabilities: None,
                    reconnect: None,
                    resume: None,
                    completed: None,
                },
            }),
            None,
        )
        .expect("hello_ack encodes")
    }

    fn tool_request_frame(request_id: &str) -> String {
        encode_relay_message(
            &RelayMessage::ToolRequest(ToolRequestMessage {
                envelope: RelayEnvelope::new("ws1"),
                request_id: request_id.to_owned(),
                operation: "herdr_inspect".to_owned(),
                arguments: Some(Map::new()),
                timeout_ms: Some(Number::from(5_000)),
                contract_epoch: Some(Number::from(2)),
                contract_hash: Some(format!("sha256:{}", "a".repeat(64))),
                idempotency_key: None,
                trace: None,
            }),
            None,
        )
        .expect("tool request encodes")
    }

    fn status_query_frame() -> String {
        encode_relay_message(
            &RelayMessage::Status(StatusMessage {
                envelope: RelayEnvelope::new("ws1"),
                fields: StatusFields {
                    query: Some(true),
                    ..StatusFields::default()
                },
            }),
            None,
        )
        .expect("status query encodes")
    }

    fn decode_send(command: WebSocketCommand) -> RelayMessage {
        match command {
            WebSocketCommand::SendText { frame, .. } => {
                decode_relay_frame(&frame, None).expect("sent frame decodes")
            }
            other => panic!("expected SendText, got {other:?}"),
        }
    }

    async fn bring_online(control: &mut FakeSocketControl) {
        send_open(control);
        assert!(matches!(
            decode_send(next_command(control).await),
            RelayMessage::Hello(_)
        ));
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: hello_ack_success(),
            })
            .expect("hello_ack accepted");
    }

    fn send_closed(control: &FakeSocketControl, code: u16, reason: &str) {
        control
            .event_tx
            .send(WebSocketEvent::Closed {
                attempt_id: control.attempt_id,
                code,
                reason: reason.to_owned(),
            })
            .expect("closed event accepted");
    }

    #[tokio::test]
    async fn start_online_heartbeat_then_graceful_stop() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: Some(json!({"ok": true})),
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(40, 100, 2_000, 262_144, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));

        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(15), control.command_rx.recv())
                .await
                .is_err(),
            "online entry must not emit an immediate heartbeat"
        );
        assert!(matches!(
            decode_send(next_command(&mut control).await),
            RelayMessage::Heartbeat(_)
        ));

        stop_tx.send(true).unwrap();
        assert!(matches!(
            next_command(&mut control).await,
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn handshake_timeout_reconnects_with_new_attempt_and_aborts_old_handle() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: None,
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 20, 3_000, 262_144, None),
            make_runner(runtime),
            Arc::clone(&connector),
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));

        let mut first = next_control(&mut controls).await;
        send_open(&first);
        assert!(matches!(
            decode_send(next_command(&mut first).await),
            RelayMessage::Hello(_)
        ));
        assert!(matches!(
            next_command(&mut first).await,
            WebSocketCommand::Terminate { .. }
        ));

        let mut second = next_control(&mut controls).await;
        assert_ne!(first.attempt_id, second.attempt_id);
        assert!(first.aborted.load(Ordering::Acquire));
        bring_online(&mut second).await;
        assert_eq!(
            connector.attempts(),
            vec![first.attempt_id, second.attempt_id]
        );

        stop_tx.send(true).unwrap();
        assert!(matches!(
            next_command(&mut second).await,
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        send_closed(&second, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn superseded_close_exits_without_reconnect() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: None,
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            Arc::clone(&connector),
        );
        let (_stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        send_closed(&control, WS_CLOSE_SUPERSEDED, "newer link");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Superseded);
        assert_eq!(connector.attempts().len(), 1);
    }

    #[tokio::test]
    async fn tool_request_round_trips_through_runner_and_runtime() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: Some(json!({"answer": 42})),
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: tool_request_frame("r1"),
            })
            .unwrap();
        assert!(matches!(
            decode_send(next_command(&mut control).await),
            RelayMessage::ToolResult(result)
                if result.request_id == "r1" && result.result == Some(json!({"answer": 42}))
        ));

        stop_tx.send(true).unwrap();
        assert!(matches!(
            next_command(&mut control).await,
            WebSocketCommand::Close { .. }
        ));
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn oversized_inbound_sends_correlated_payload_too_large() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: None,
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 1_024, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        let oversized = json!({
            "protocol_version": 1,
            "kind": "tool_request",
            "workstation_id": "ws1",
            "request_id": "too-big",
            "operation": "herdr_inspect",
            "arguments": {"pad": "x".repeat(2_000)}
        })
        .to_string();
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: oversized,
            })
            .unwrap();
        assert!(matches!(
            decode_send(next_command(&mut control).await),
            RelayMessage::ToolError(error)
                if error.request_id == "too-big"
                    && error.code == "payload_too_large"
                    && error.delivery_state == Some(DeliveryState::NotDelivered)
        ));

        stop_tx.send(true).unwrap();
        assert!(matches!(
            next_command(&mut control).await,
            WebSocketCommand::Close { .. }
        ));
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn graceful_stop_drains_existing_request_before_close() {
        let gate = Arc::new(Notify::new());
        let started = Arc::new(Semaphore::new(0));
        let runtime = Arc::new(MockRuntime::blocked(
            RuntimeToolResult::Success {
                result: Some(json!({"drained": true})),
            },
            Arc::clone(&gate),
            Arc::clone(&started),
        ));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(250),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: tool_request_frame("drain"),
            })
            .unwrap();
        started.acquire().await.unwrap().forget();

        stop_tx.send(true).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), control.command_rx.recv())
                .await
                .is_err(),
            "stop must not close while an in-flight request can still drain"
        );
        gate.notify_waiters();
        assert!(matches!(
            decode_send(next_command(&mut control).await),
            RelayMessage::ToolResult(result) if result.request_id == "drain"
        ));
        assert!(matches!(
            next_command(&mut control).await,
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn drain_deadline_closes_then_rejects_leftover_locally() {
        let gate = Arc::new(Notify::new());
        let started = Arc::new(Semaphore::new(0));
        let runtime = Arc::new(MockRuntime::blocked(
            RuntimeToolResult::Success { result: None },
            gate,
            Arc::clone(&started),
        ));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(30),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: tool_request_frame("leftover"),
            })
            .unwrap();
        started.acquire().await.unwrap().forget();
        stop_tx.send(true).unwrap();

        assert!(matches!(
            next_command(&mut control).await,
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        assert!(matches!(
            decode_send(next_command(&mut control).await),
            RelayMessage::ToolError(error)
                if error.request_id == "leftover"
                    && error.code == "link_stopping"
                    && error.delivery_state == Some(DeliveryState::NotDelivered)
        ));
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn max_reconnect_and_stop_during_connect_are_bounded() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: None,
        }));
        let (failing_connector, _controls) = FakeConnector::new([
            ConnectPlan::Fail(Duration::ZERO),
            ConnectPlan::Fail(Duration::ZERO),
        ]);
        let failing_io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, Some(1)),
            make_runner(Arc::clone(&runtime)),
            Arc::clone(&failing_connector),
        );
        let (_stop_tx, stop_rx) = watch::channel(false);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), failing_io.run(stop_rx))
                .await
                .unwrap()
                .unwrap(),
            LinkExitKind::MaxReconnect
        );
        assert_eq!(failing_connector.attempts().len(), 2);

        let (slow_connector, mut slow_controls) =
            FakeConnector::new([ConnectPlan::Success(Duration::from_millis(250))]);
        let slow_io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            slow_connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(slow_io.run(stop_rx));
        tokio::task::yield_now().await;
        stop_tx.send(true).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            LinkExitKind::Stopped
        );
        let stale_control =
            tokio::time::timeout(Duration::from_millis(300), slow_controls.recv()).await;
        assert!(
            !matches!(stale_control, Ok(Some(_))),
            "aborted connect must not materialize a stale socket handle"
        );
    }

    #[tokio::test]
    async fn slow_status_health_does_not_block_stop() {
        let health_gate = Arc::new(Notify::new());
        let health_started = Arc::new(Notify::new());
        let runtime = Arc::new(MockRuntime::blocked_health(
            Arc::clone(&health_gate),
            Arc::clone(&health_started),
        ));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            connector,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut control = next_control(&mut controls).await;
        bring_online(&mut control).await;

        let health_begun = health_started.notified();
        control
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: control.attempt_id,
                text: status_query_frame(),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), health_begun)
            .await
            .expect("status probe must start without occupying the I/O loop");

        let started = Instant::now();
        stop_tx.send(true).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(400), control.command_rx.recv())
                .await
                .expect("stop must close while health is still in flight")
                .expect("close command"),
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "stop must not wait for a slow get_health"
        );
        health_gate.notify_waiters();
        send_closed(&control, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }

    #[tokio::test]
    async fn full_command_channel_recycles_socket_without_blocking_the_loop() {
        let runtime = Arc::new(MockRuntime::immediate(RuntimeToolResult::Success {
            result: None,
        }));
        let (connector, mut controls) = FakeConnector::new([]);
        let io = LinkIoLoop::with_connector(
            io_config(100),
            make_core(1_000, 100, 3_000, 262_144, None),
            make_runner(runtime),
            Arc::clone(&connector),
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(io.run(stop_rx));
        let mut first = next_control(&mut controls).await;
        bring_online(&mut first).await;
        connector.refuse_further_commands();

        let started = Instant::now();
        first
            .event_tx
            .send(WebSocketEvent::Text {
                attempt_id: first.attempt_id,
                text: status_query_frame(),
            })
            .unwrap();
        let mut second =
            tokio::time::timeout(Duration::from_millis(500), next_control(&mut controls))
                .await
                .expect("full command queue must recycle the socket instead of parking the loop");
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "command backpressure must fail closed immediately"
        );
        assert_ne!(first.attempt_id, second.attempt_id);
        assert!(first.aborted.load(Ordering::Acquire));

        connector.allow_commands();
        bring_online(&mut second).await;
        stop_tx.send(true).unwrap();
        assert!(matches!(
            next_command(&mut second).await,
            WebSocketCommand::Close {
                code: WS_CLOSE_NORMAL,
                ..
            }
        ));
        send_closed(&second, WS_CLOSE_NORMAL, "done");
        assert_eq!(task.await.unwrap().unwrap(), LinkExitKind::Stopped);
    }
}
