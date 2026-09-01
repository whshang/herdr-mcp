//! Concrete async WebSocket driver for the staged Rust Link transport core.
//!
//! This layer owns only socket I/O. It composes the same URL/subprotocol
//! handshake as the Node client, keeps the reversible link secret out of URLs
//! and error text, bounds WebSocket memory, and turns socket activity into
//! attempt-id-fenced events that can be fed into [`LinkTransportCore`].
//!
//! It deliberately does not own runtime dispatch, daemon configuration,
//! launchd/service mutation, or production activation.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_tungstenite::{
    WebSocketStream, client_async_tls_with_config, connect_async_tls_with_config,
};
use url::Url;

use super::proxy::{connect_via_http_proxy, resolve_link_proxy, wss_target};
use super::transport::{
    LINK_DEFAULT_MAX_FRAME_BYTES, LinkTransportCore, SocketAttemptId, TransportAction,
    TransportError,
};
use crate::relay::protocol::HelloMessage;

pub const LINK_SUBPROTOCOL: &str = "herdr-link.v1";
const DEVICE_NAME_HEADER: &str = "x-herdr-device-name-b64";
pub const LINK_AUTH_PROTOCOL_PREFIX: &str = "herdr-auth.";
pub const DEFAULT_WS_HARD_LIMIT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_WS_WRITE_BUFFER_BYTES: usize = 16 * 1024;
pub const DEFAULT_WS_MAX_WRITE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_SOCKET_COMMAND_CAPACITY: usize = 64;
pub const DEFAULT_SOCKET_EVENT_CAPACITY: usize = 64;
pub const DEFAULT_FORCE_CLOSE_MS: u64 = 2_000;
pub const DEFAULT_MAX_CLOSE_REASON_BYTES: usize = 120;
pub const ABNORMAL_CLOSE_CODE: u16 = 1006;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketDriverConfig {
    pub hard_max_message_bytes: usize,
    pub hard_max_frame_bytes: usize,
    pub write_buffer_bytes: usize,
    pub max_write_buffer_bytes: usize,
    pub command_capacity: usize,
    pub event_capacity: usize,
    pub force_close_ms: u64,
}

impl Default for SocketDriverConfig {
    fn default() -> Self {
        Self {
            hard_max_message_bytes: DEFAULT_WS_HARD_LIMIT_BYTES,
            hard_max_frame_bytes: DEFAULT_WS_HARD_LIMIT_BYTES,
            write_buffer_bytes: DEFAULT_WS_WRITE_BUFFER_BYTES,
            max_write_buffer_bytes: DEFAULT_WS_MAX_WRITE_BUFFER_BYTES,
            command_capacity: DEFAULT_SOCKET_COMMAND_CAPACITY,
            event_capacity: DEFAULT_SOCKET_EVENT_CAPACITY,
            force_close_ms: DEFAULT_FORCE_CLOSE_MS,
        }
    }
}

impl SocketDriverConfig {
    fn websocket_config(self) -> WebSocketConfig {
        WebSocketConfig::default()
            .write_buffer_size(self.write_buffer_bytes)
            .max_write_buffer_size(self.max_write_buffer_bytes)
            .max_message_size(Some(self.hard_max_message_bytes))
            .max_frame_size(Some(self.hard_max_frame_bytes))
    }

    fn normalized(self) -> Self {
        let protocol_floor = LINK_DEFAULT_MAX_FRAME_BYTES.saturating_add(1);
        let hard_max_message_bytes = self.hard_max_message_bytes.max(protocol_floor);
        let hard_max_frame_bytes = self.hard_max_frame_bytes.max(protocol_floor);
        let write_buffer_bytes = self.write_buffer_bytes.max(1);
        let max_write_buffer_bytes = self
            .max_write_buffer_bytes
            .max(write_buffer_bytes.saturating_add(hard_max_message_bytes));
        Self {
            hard_max_message_bytes,
            hard_max_frame_bytes,
            write_buffer_bytes,
            max_write_buffer_bytes,
            command_capacity: self.command_capacity.max(1),
            event_capacity: self.event_capacity.max(1),
            force_close_ms: self.force_close_ms.max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SocketDriverError {
    InvalidUrl,
    InsecureScheme,
    InvalidSubprotocol,
    ConnectFailed,
    NegotiatedProtocolMissing,
    NegotiatedProtocolMismatch,
    CommandChannelClosed,
    CommandChannelFull,
    MissingHello,
    Transport(TransportError),
}

impl From<TransportError> for SocketDriverError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketCommand {
    SendText {
        attempt_id: SocketAttemptId,
        frame: String,
    },
    SendPing {
        attempt_id: SocketAttemptId,
    },
    Close {
        attempt_id: SocketAttemptId,
        code: u16,
        reason: String,
    },
    Terminate {
        attempt_id: SocketAttemptId,
    },
}

impl WebSocketCommand {
    pub fn attempt_id(&self) -> SocketAttemptId {
        match self {
            Self::SendText { attempt_id, .. }
            | Self::SendPing { attempt_id, .. }
            | Self::Close { attempt_id, .. }
            | Self::Terminate { attempt_id } => *attempt_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketEvent {
    Opened {
        attempt_id: SocketAttemptId,
        selected_protocol: String,
    },
    Text {
        attempt_id: SocketAttemptId,
        text: String,
    },
    SocketErrorObserved {
        attempt_id: SocketAttemptId,
    },
    TransportLiveness {
        attempt_id: SocketAttemptId,
    },
    Closed {
        attempt_id: SocketAttemptId,
        code: u16,
        reason: String,
    },
}

pub struct SocketAttemptHandle {
    attempt_id: SocketAttemptId,
    command_tx: mpsc::Sender<WebSocketCommand>,
    event_rx: mpsc::Receiver<WebSocketEvent>,
    task: JoinHandle<()>,
}

impl SocketAttemptHandle {
    pub fn attempt_id(&self) -> SocketAttemptId {
        self.attempt_id
    }

    pub async fn next_event(&mut self) -> Option<WebSocketEvent> {
        self.event_rx.recv().await
    }

    /// Execute only the socket-bound subset of a reactor action.
    ///
    /// Returns `false` for timer/reconnect/higher-layer actions so the caller
    /// can route those elsewhere without duplicating socket policy here.
    ///
    /// Command delivery is non-blocking: a full or closed bounded channel fails
    /// closed immediately so the outer I/O loop can recycle the socket instead of
    /// parking on `send().await`.
    pub async fn execute_action(
        &self,
        action: &TransportAction,
    ) -> Result<bool, SocketDriverError> {
        let Some(command) = command_for_action(action) else {
            return Ok(false);
        };
        if command.attempt_id() != self.attempt_id {
            return Ok(false);
        }
        match self.command_tx.try_send(command) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(_)) => Err(SocketDriverError::CommandChannelFull),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(SocketDriverError::CommandChannelClosed)
            }
        }
    }

    #[cfg(test)]
    fn for_test(
        attempt_id: SocketAttemptId,
        command_tx: mpsc::Sender<WebSocketCommand>,
        event_rx: mpsc::Receiver<WebSocketEvent>,
    ) -> Self {
        Self {
            attempt_id,
            command_tx,
            event_rx,
            task: tokio::spawn(async {}),
        }
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for SocketAttemptHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn build_link_auth_protocol(link_token: &str) -> String {
    let mut out = String::with_capacity(LINK_AUTH_PROTOCOL_PREFIX.len() + link_token.len() * 2);
    out.push_str(LINK_AUTH_PROTOCOL_PREFIX);
    for byte in link_token.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub fn build_link_protocols(protocol_id: &str, link_token: &str) -> Vec<String> {
    if link_token.is_empty() {
        vec![protocol_id.to_owned()]
    } else {
        vec![protocol_id.to_owned(), build_link_auth_protocol(link_token)]
    }
}

fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(*byte as char),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Node-parity edge URL construction. Only the workstation id enters the URL;
/// link credentials remain exclusively in the WebSocket subprotocol header.
pub fn build_edge_url(base_url: &str, workstation_id: &str) -> Result<String, SocketDriverError> {
    let mut url = Url::parse(base_url).map_err(|_| SocketDriverError::InvalidUrl)?;
    let base_path = url.path().trim_end_matches('/');
    let encoded = encode_uri_component(workstation_id);
    if !base_path.ends_with(&format!("/{encoded}")) {
        // `Url::path()` reports the origin root as `/`; after trimming it is
        // empty, but keep the root case explicit so the default `/ws` mapping
        // remains obvious and regression-proof.
        let prefix = if base_path.is_empty() || base_path == "/" {
            "/ws"
        } else {
            base_path
        };
        url.set_path(&format!("{prefix}/{encoded}"));
    }

    let retained = url
        .query_pairs()
        .filter(|(key, _)| key != "workstation_id" && key != "link_token")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !retained.is_empty() {
        url.query_pairs_mut().extend_pairs(retained);
    }
    Ok(url.to_string())
}

pub fn redact_url(raw: &str) -> String {
    if let Ok(mut url) = Url::parse(raw) {
        let pairs = url
            .query_pairs()
            .map(|(key, value)| {
                if key == "link_token" {
                    (key.into_owned(), "***".to_owned())
                } else {
                    (key.into_owned(), value.into_owned())
                }
            })
            .collect::<Vec<_>>();
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
        return url.to_string();
    }

    let mut result = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find("link_token=") {
        let (head, after_head) = rest.split_at(index);
        result.push_str(head);
        result.push_str("link_token=***");
        let after_value = &after_head["link_token=".len()..];
        match after_value.find('&') {
            Some(end) => {
                rest = &after_value[end..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    result.push_str(rest);
    result
}

pub fn validate_wss_url(raw: &str) -> Result<Url, SocketDriverError> {
    let url = Url::parse(raw).map_err(|_| SocketDriverError::InvalidUrl)?;
    if url.scheme() != "wss" {
        return Err(SocketDriverError::InsecureScheme);
    }
    Ok(url)
}

/// Rustls 0.23 requires a process-level crypto provider whenever provider
/// selection is not unambiguous. The wider Herdr graph currently selects
/// aws-lc-rs through reqwest, but the WebSocket driver must not rely on that
/// unrelated dependency forever. Install the same provider explicitly when
/// the process has not already selected one; preserve an existing choice.
fn ensure_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

fn client_request(
    edge_url: &str,
    application_protocol: &str,
    link_token: &str,
    device_name: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, SocketDriverError> {
    validate_wss_url(edge_url)?;
    let mut request = edge_url
        .into_client_request()
        .map_err(|_| SocketDriverError::InvalidUrl)?;
    let protocols = build_link_protocols(application_protocol, link_token);
    let header = HeaderValue::from_str(&protocols.join(", "))
        .map_err(|_| SocketDriverError::InvalidSubprotocol)?;
    request.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, header);
    if let Some(name) = device_name.and_then(crate::device_name::normalize_device_display_name) {
        let encoded = URL_SAFE_NO_PAD.encode(name.as_bytes());
        if let Ok(header) = HeaderValue::from_str(&encoded) {
            request.headers_mut().insert(DEVICE_NAME_HEADER, header);
        }
    }
    Ok(request)
}

fn verify_selected_protocol(
    selected: Option<&HeaderValue>,
    application_protocol: &str,
) -> Result<String, SocketDriverError> {
    let selected = selected.ok_or(SocketDriverError::NegotiatedProtocolMissing)?;
    let selected = selected
        .to_str()
        .map_err(|_| SocketDriverError::NegotiatedProtocolMismatch)?
        .trim();
    if selected != application_protocol {
        return Err(SocketDriverError::NegotiatedProtocolMismatch);
    }
    Ok(selected.to_owned())
}

pub fn command_for_action(action: &TransportAction) -> Option<WebSocketCommand> {
    match action {
        TransportAction::SendFrame { attempt_id, frame } => Some(WebSocketCommand::SendText {
            attempt_id: *attempt_id,
            frame: frame.clone(),
        }),
        TransportAction::TransportPingDue { attempt_id } => Some(WebSocketCommand::SendPing {
            attempt_id: *attempt_id,
        }),
        TransportAction::CloseSocket {
            attempt_id,
            code,
            reason,
        } => Some(WebSocketCommand::Close {
            attempt_id: *attempt_id,
            code: *code,
            reason: (*reason).to_owned(),
        }),
        TransportAction::TerminateSocket { attempt_id } => Some(WebSocketCommand::Terminate {
            attempt_id: *attempt_id,
        }),
        _ => None,
    }
}

/// Feed one concrete socket event back into the transport reactor.
///
/// The caller supplies `hello` only for `Opened`; this keeps runtime identity
/// construction outside the socket layer. All other events are translated
/// without any runtime or credential knowledge.
pub fn feed_socket_event(
    core: &mut LinkTransportCore,
    event: WebSocketEvent,
    hello: Option<HelloMessage>,
    now_ms: i64,
    rng_sample: f64,
) -> Result<Vec<TransportAction>, SocketDriverError> {
    match event {
        WebSocketEvent::Opened { attempt_id, .. } => {
            let hello = hello.ok_or(SocketDriverError::MissingHello)?;
            Ok(core.socket_opened(attempt_id, hello)?)
        }
        WebSocketEvent::Text { attempt_id, text } => {
            Ok(core.frame_received(attempt_id, &text, now_ms, rng_sample)?)
        }
        WebSocketEvent::TransportLiveness { attempt_id } => {
            if core.active_attempt() == Some(attempt_id) {
                core.transport_liveness_observed(now_ms);
            }
            Ok(Vec::new())
        }
        WebSocketEvent::SocketErrorObserved { attempt_id } => Ok(core.socket_error(attempt_id)),
        WebSocketEvent::Closed {
            attempt_id,
            code,
            reason,
        } => Ok(core.socket_closed(attempt_id, code, reason, now_ms, rng_sample)?),
    }
}

/// Connect one production socket attempt. Connection failures are deliberately
/// collapsed to a non-secret error; tungstenite handshake diagnostics may
/// contain request metadata and must never expose the reversible auth protocol.
pub async fn connect_socket_attempt(
    edge_url: &str,
    application_protocol: &str,
    link_token: &str,
    device_name: Option<&str>,
    attempt_id: SocketAttemptId,
    config: SocketDriverConfig,
) -> Result<SocketAttemptHandle, SocketDriverError> {
    ensure_rustls_crypto_provider();
    let config = config.normalized();
    let request = client_request(edge_url, application_protocol, link_token, device_name)?;
    let (socket, response) = if let Some(proxy) = resolve_link_proxy() {
        let (target_host, target_port) =
            wss_target(edge_url).ok_or(SocketDriverError::InvalidUrl)?;
        let tcp = connect_via_http_proxy(&proxy.url, &target_host, target_port)
            .await
            .map_err(|_| SocketDriverError::ConnectFailed)?;
        client_async_tls_with_config(request, tcp, Some(config.websocket_config()), None)
            .await
            .map_err(|_| SocketDriverError::ConnectFailed)?
    } else {
        connect_async_tls_with_config(request, Some(config.websocket_config()), true, None)
            .await
            .map_err(|_| SocketDriverError::ConnectFailed)?
    };

    let selected_protocol = verify_selected_protocol(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        application_protocol,
    )?;

    let (command_tx, command_rx) = mpsc::channel(config.command_capacity);
    let (event_tx, event_rx) = mpsc::channel(config.event_capacity);
    event_tx
        .send(WebSocketEvent::Opened {
            attempt_id,
            selected_protocol,
        })
        .await
        .map_err(|_| SocketDriverError::CommandChannelClosed)?;
    let task = tokio::spawn(run_connected_socket(
        socket, attempt_id, command_rx, event_tx, config,
    ));

    Ok(SocketAttemptHandle {
        attempt_id,
        command_tx,
        event_rx,
        task,
    })
}

async fn send_event(event_tx: &mpsc::Sender<WebSocketEvent>, event: WebSocketEvent) -> bool {
    event_tx.send(event).await.is_ok()
}

fn bounded_close_reason(reason: &str) -> String {
    if reason.len() <= DEFAULT_MAX_CLOSE_REASON_BYTES {
        return reason.to_owned();
    }
    let mut end = DEFAULT_MAX_CLOSE_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

async fn emit_error_then_closed(
    event_tx: &mpsc::Sender<WebSocketEvent>,
    attempt_id: SocketAttemptId,
    reason: &'static str,
) {
    if !send_event(event_tx, WebSocketEvent::SocketErrorObserved { attempt_id }).await {
        return;
    }
    let _ = send_event(
        event_tx,
        WebSocketEvent::Closed {
            attempt_id,
            code: ABNORMAL_CLOSE_CODE,
            reason: reason.to_owned(),
        },
    )
    .await;
}

async fn run_connected_socket<S>(
    mut socket: WebSocketStream<S>,
    attempt_id: SocketAttemptId,
    mut command_rx: mpsc::Receiver<WebSocketCommand>,
    event_tx: mpsc::Sender<WebSocketEvent>,
    config: SocketDriverConfig,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let close_sleep = sleep(Duration::from_secs(365 * 24 * 60 * 60));
    tokio::pin!(close_sleep);
    let mut close_armed = false;

    loop {
        tokio::select! {
            _ = &mut close_sleep, if close_armed => {
                let _ = send_event(
                    &event_tx,
                    WebSocketEvent::Closed {
                        attempt_id,
                        code: ABNORMAL_CLOSE_CODE,
                        reason: "close handshake timeout".to_owned(),
                    },
                ).await;
                break;
            }
            command = command_rx.recv() => {
                let Some(command) = command else {
                    let _ = send_event(
                        &event_tx,
                        WebSocketEvent::Closed {
                            attempt_id,
                            code: ABNORMAL_CLOSE_CODE,
                            reason: "driver command channel closed".to_owned(),
                        },
                    ).await;
                    break;
                };
                if command.attempt_id() != attempt_id {
                    continue;
                }
                match command {
                    WebSocketCommand::SendText { frame, .. } => {
                        if socket.send(Message::Text(frame.into())).await.is_err() {
                            emit_error_then_closed(&event_tx, attempt_id, "socket send failed").await;
                            break;
                        }
                    }
                    WebSocketCommand::SendPing { .. } => {
                        if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                            emit_error_then_closed(&event_tx, attempt_id, "socket ping failed").await;
                            break;
                        }
                    }
                    WebSocketCommand::Close { code, reason, .. } => {
                        let frame = CloseFrame {
                            code: CloseCode::from(code),
                            reason: bounded_close_reason(&reason).into(),
                        };
                        if socket.close(Some(frame)).await.is_err() {
                            emit_error_then_closed(&event_tx, attempt_id, "socket close failed").await;
                            break;
                        }
                        close_armed = true;
                        close_sleep.as_mut().reset(
                            Instant::now() + Duration::from_millis(config.force_close_ms),
                        );
                    }
                    WebSocketCommand::Terminate { .. } => {
                        let _ = send_event(
                            &event_tx,
                            WebSocketEvent::Closed {
                                attempt_id,
                                code: ABNORMAL_CLOSE_CODE,
                                reason: "socket terminated".to_owned(),
                            },
                        ).await;
                        break;
                    }
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if !send_event(
                            &event_tx,
                            WebSocketEvent::Text {
                                attempt_id,
                                text: text.to_string(),
                            },
                        ).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if !send_event(
                            &event_tx,
                            WebSocketEvent::Text { attempt_id, text },
                        ).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) = frame
                            .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                            .unwrap_or((ABNORMAL_CLOSE_CODE, String::new()));
                        // Tungstenite queues the RFC close reply when the peer
                        // close is read. Flush that automatic acknowledgement
                        // before publishing Closed and dropping the stream.
                        let _ = socket.flush().await;
                        let _ = send_event(
                            &event_tx,
                            WebSocketEvent::Closed { attempt_id, code, reason },
                        ).await;
                        break;
                    }
                    Some(Ok(Message::Ping(_))) => {
                        // Tungstenite answers inbound ping with pong locally.
                    }
                    Some(Ok(Message::Pong(_))) => {
                        if !send_event(
                            &event_tx,
                            WebSocketEvent::TransportLiveness { attempt_id },
                        ).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Frame(_))) => {
                        // Raw frames are an internal tungstenite detail and never
                        // enter the application Relay contract.
                    }
                    Some(Err(_)) => {
                        emit_error_then_closed(&event_tx, attempt_id, "socket read failed").await;
                        break;
                    }
                    None => {
                        let _ = send_event(
                            &event_tx,
                            WebSocketEvent::Closed {
                                attempt_id,
                                code: ABNORMAL_CLOSE_CODE,
                                reason: "socket eof".to_owned(),
                            },
                        ).await;
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::Number;
    use tokio::io::duplex;
    use tokio::sync::mpsc;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::protocol::{Message, Role};

    use super::*;
    use crate::link::backoff::ExponentialBackoff;
    use crate::link::lifecycle::ConnectionPhase;
    use crate::link::transport::TransportConfig;
    use crate::relay::protocol::RuntimeContractInfo;
    use crate::relay::wire::build_hello_message;

    fn hello() -> HelloMessage {
        build_hello_message(
            "w5C",
            "boot1",
            "0.4.0-alpha.6",
            vec!["relay.request".to_owned()],
            RuntimeContractInfo {
                runtime_version: "0.4.0-alpha.6".to_owned(),
                runtime_commit: None,
                runtime_generation: Some("g1".to_owned()),
                contract_epoch: Number::from(2),
                contract_hash: Some("sha256:test".to_owned()),
                herdr_version: Some("0.8.2".to_owned()),
                herdr_protocol: Some("20".to_owned()),
            },
            Number::from(1_000),
        )
    }

    fn core() -> LinkTransportCore {
        LinkTransportCore::new(
            "w5C",
            Some(3),
            ExponentialBackoff::default(),
            TransportConfig::default(),
        )
    }

    #[test]
    fn helpers_match_node_socket_oracle_and_keep_token_out_of_url() {
        assert_eq!(
            build_link_auth_protocol("tok-123"),
            "herdr-auth.746f6b2d313233"
        );
        assert_eq!(
            build_link_protocols(LINK_SUBPROTOCOL, "tok-123"),
            vec![
                "herdr-link.v1".to_owned(),
                "herdr-auth.746f6b2d313233".to_owned()
            ]
        );
        assert_eq!(
            build_link_protocols(LINK_SUBPROTOCOL, ""),
            vec!["herdr-link.v1".to_owned()]
        );

        let built = build_edge_url(
            "wss://edge.test/ws?workstation_id=old&link_token=tok-123&keep=yes",
            "w5C",
        )
        .unwrap();
        assert_eq!(built, "wss://edge.test/ws/w5C?keep=yes");
        assert!(!built.contains("tok-123"));
        assert!(!built.contains("link_token"));
        assert!(!built.contains("workstation_id"));
        assert_eq!(
            build_edge_url("wss://edge.test/ws/w5C", "w5C").unwrap(),
            "wss://edge.test/ws/w5C"
        );
        assert_eq!(
            build_edge_url("wss://edge.test", "a b:c").unwrap(),
            "wss://edge.test/ws/a%20b%3Ac"
        );
        assert_eq!(
            build_edge_url("wss://edge.test/", "a b:c").unwrap(),
            "wss://edge.test/ws/a%20b%3Ac"
        );
    }

    #[test]
    fn redaction_and_wss_gate_never_require_secret_logging() {
        let redacted = redact_url("wss://edge.test/ws?link_token=tok-123&x=1");
        assert!(!redacted.contains("tok-123"));
        assert!(redacted.contains("link_token="));
        assert_eq!(
            redact_url("broken:// url?link_token=tok-123&x=1"),
            "broken:// url?link_token=***&x=1"
        );
        assert!(validate_wss_url("wss://edge.test/ws").is_ok());
        assert_eq!(
            validate_wss_url("ws://edge.test/ws"),
            Err(SocketDriverError::InsecureScheme)
        );
    }

    #[test]
    fn rustls_crypto_provider_is_explicitly_available_before_wss_connect() {
        ensure_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        let _ = rustls::ClientConfig::builder();
    }

    #[test]
    fn websocket_hard_cap_stays_above_relay_budget_and_queues_are_bounded() {
        let config = SocketDriverConfig {
            hard_max_message_bytes: 1,
            hard_max_frame_bytes: 1,
            write_buffer_bytes: 0,
            max_write_buffer_bytes: 0,
            command_capacity: 0,
            event_capacity: 0,
            force_close_ms: 0,
        }
        .normalized();
        assert!(config.hard_max_message_bytes > LINK_DEFAULT_MAX_FRAME_BYTES);
        assert!(config.hard_max_frame_bytes > LINK_DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(config.command_capacity, 1);
        assert_eq!(config.event_capacity, 1);
        assert_eq!(config.force_close_ms, 1);
        assert!(config.max_write_buffer_bytes > config.write_buffer_bytes);
    }

    #[test]
    fn request_header_contains_app_then_hex_auth_protocol() {
        let request = client_request(
            "wss://edge.test/ws/w5C",
            LINK_SUBPROTOCOL,
            "tok-123",
            Some("青闲 MacBook Air"),
        )
        .unwrap();
        assert_eq!(
            request
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .unwrap()
                .to_str()
                .unwrap(),
            "herdr-link.v1, herdr-auth.746f6b2d313233"
        );
        assert_eq!(
            request
                .headers()
                .get(DEVICE_NAME_HEADER)
                .unwrap()
                .to_str()
                .unwrap(),
            URL_SAFE_NO_PAD.encode("青闲 MacBook Air".as_bytes())
        );
        assert!(!request.uri().to_string().contains("tok-123"));
    }

    #[test]
    fn selected_subprotocol_is_required_and_must_be_the_application_protocol() {
        let good = HeaderValue::from_static("herdr-link.v1");
        let wrong = HeaderValue::from_static("herdr-auth.00");
        assert_eq!(
            verify_selected_protocol(Some(&good), LINK_SUBPROTOCOL).unwrap(),
            LINK_SUBPROTOCOL
        );
        assert_eq!(
            verify_selected_protocol(None, LINK_SUBPROTOCOL),
            Err(SocketDriverError::NegotiatedProtocolMissing)
        );
        assert_eq!(
            verify_selected_protocol(Some(&wrong), LINK_SUBPROTOCOL),
            Err(SocketDriverError::NegotiatedProtocolMismatch)
        );
    }

    #[test]
    fn feed_socket_events_drives_existing_attempt_fence_and_handshake() {
        let mut core = core();
        let start = core.start().unwrap();
        let attempt_id = match start.as_slice() {
            [TransportAction::OpenSocket { attempt_id }] => *attempt_id,
            other => panic!("unexpected start actions: {other:?}"),
        };
        let opened = feed_socket_event(
            &mut core,
            WebSocketEvent::Opened {
                attempt_id,
                selected_protocol: LINK_SUBPROTOCOL.to_owned(),
            },
            Some(hello()),
            1_000,
            0.5,
        )
        .unwrap();
        assert_eq!(core.phase(), ConnectionPhase::Handshake);
        assert!(matches!(
            opened.as_slice(),
            [
                TransportAction::SendFrame { .. },
                TransportAction::ArmHandshakeTimeout { .. }
            ]
        ));

        let stale = feed_socket_event(
            &mut core,
            WebSocketEvent::Closed {
                attempt_id: SocketAttemptId(attempt_id.0 + 99),
                code: 1006,
                reason: "stale".to_owned(),
            },
            None,
            1_100,
            0.5,
        )
        .unwrap();
        assert!(stale.is_empty());
        assert_eq!(core.phase(), ConnectionPhase::Handshake);
    }

    #[tokio::test]
    async fn connected_task_maps_text_binary_and_close_without_unbounded_channels() {
        let config = SocketDriverConfig::default().normalized();
        let (client_io, server_io) = duplex(64 * 1024);
        let client = WebSocketStream::from_raw_socket(
            client_io,
            Role::Client,
            Some(config.websocket_config()),
        )
        .await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (command_tx, command_rx) = mpsc::channel(config.command_capacity);
        let (event_tx, mut event_rx) = mpsc::channel(config.event_capacity);
        let attempt_id = SocketAttemptId(7);
        let task = tokio::spawn(run_connected_socket(
            client, attempt_id, command_rx, event_tx, config,
        ));

        command_tx
            .send(WebSocketCommand::SendText {
                attempt_id,
                frame: "client-frame".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::Text("client-frame".into())
        );

        server
            .send(Message::Text("server-text".into()))
            .await
            .unwrap();
        server
            .send(Message::Binary(vec![b'{', 0xff, b'}'].into()))
            .await
            .unwrap();
        server
            .close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "bye".into(),
            }))
            .await
            .unwrap();

        assert_eq!(
            event_rx.recv().await.unwrap(),
            WebSocketEvent::Text {
                attempt_id,
                text: "server-text".to_owned(),
            }
        );
        match event_rx.recv().await.unwrap() {
            WebSocketEvent::Text {
                attempt_id: got,
                text,
            } => {
                assert_eq!(got, attempt_id);
                assert_eq!(text, "{�}");
            }
            other => panic!("unexpected binary event: {other:?}"),
        }
        assert_eq!(
            event_rx.recv().await.unwrap(),
            WebSocketEvent::Closed {
                attempt_id,
                code: 1001,
                reason: "bye".to_owned(),
            }
        );
        match server.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), 1001);
                assert_eq!(frame.reason, "bye");
            }
            other => panic!("expected close acknowledgement, got {other:?}"),
        }
        task.await.unwrap();
    }

    #[tokio::test]
    async fn terminate_is_attempt_scoped_and_emits_one_abnormal_close() {
        let config = SocketDriverConfig::default().normalized();
        let (client_io, server_io) = duplex(16 * 1024);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (command_tx, command_rx) = mpsc::channel(config.command_capacity);
        let (event_tx, mut event_rx) = mpsc::channel(config.event_capacity);
        let attempt_id = SocketAttemptId(11);
        let task = tokio::spawn(run_connected_socket(
            client, attempt_id, command_rx, event_tx, config,
        ));

        command_tx
            .send(WebSocketCommand::Terminate {
                attempt_id: SocketAttemptId(12),
            })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert!(event_rx.try_recv().is_err());

        command_tx
            .send(WebSocketCommand::Terminate { attempt_id })
            .await
            .unwrap();
        assert_eq!(
            event_rx.recv().await.unwrap(),
            WebSocketEvent::Closed {
                attempt_id,
                code: ABNORMAL_CLOSE_CODE,
                reason: "socket terminated".to_owned(),
            }
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn graceful_close_has_a_bounded_force_termination_deadline() {
        let config = SocketDriverConfig {
            force_close_ms: 10,
            ..SocketDriverConfig::default()
        }
        .normalized();
        let (client_io, server_io) = duplex(16 * 1024);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (command_tx, command_rx) = mpsc::channel(config.command_capacity);
        let (event_tx, mut event_rx) = mpsc::channel(config.event_capacity);
        let attempt_id = SocketAttemptId(13);
        let task = tokio::spawn(run_connected_socket(
            client, attempt_id, command_rx, event_tx, config,
        ));

        command_tx
            .send(WebSocketCommand::Close {
                attempt_id,
                code: 1000,
                reason: "client shutdown".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            event_rx.recv().await.unwrap(),
            WebSocketEvent::Closed {
                attempt_id,
                code: ABNORMAL_CLOSE_CODE,
                reason: "close handshake timeout".to_owned(),
            }
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn execute_action_fails_closed_on_full_or_closed_command_channel() {
        let attempt_id = SocketAttemptId(21);
        let (command_tx, command_rx) = mpsc::channel(1);
        let (_event_tx, event_rx) = mpsc::channel(1);
        let handle = SocketAttemptHandle::for_test(attempt_id, command_tx, event_rx);
        let send = TransportAction::SendFrame {
            attempt_id,
            frame: "one".to_owned(),
        };
        let started = std::time::Instant::now();
        assert_eq!(handle.execute_action(&send).await, Ok(true));
        assert_eq!(
            handle.execute_action(&send).await,
            Err(SocketDriverError::CommandChannelFull)
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "full command queue must not park the caller"
        );
        drop(command_rx);
        assert_eq!(
            handle.execute_action(&send).await,
            Err(SocketDriverError::CommandChannelClosed)
        );
    }
}
