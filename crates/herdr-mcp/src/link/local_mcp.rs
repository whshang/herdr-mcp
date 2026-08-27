//! Async local MCP runtime transport for the staged Rust Link runner.
//!
//! This mirrors `src/link/local-mcp-transport.ts`: every dispatch is one
//! sessionless JSON-RPC POST to a loopback MCP endpoint. Requests and response
//! bodies are bounded, cancellation is request-scoped, JSON-RPC ids are exact,
//! and remote bodies/messages never enter local transport errors.
//!
//! This module is staged library code only. It does not own WebSockets,
//! generation activation, daemon/service mutation, credential persistence, or
//! production cutover.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde_json::{Map, Number, Value, json};
use tokio::sync::watch;
use url::Url;

use super::request_core::RuntimeRequest;
use crate::relay::protocol::RuntimeContractInfo;

pub const LOCAL_MCP_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8772/mcp";
pub const LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const LOCAL_MCP_DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const LOCAL_MCP_MAX_TIMEOUT_MS: u64 = 120_000;
pub const LOCAL_MCP_CONTRACT_EPOCH: u64 = 2;

const LOCAL_MCP_MIN_FRAME_BYTES: usize = 64;
const LOCAL_MCP_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub mod code {
    pub const BAD_REQUEST: &str = "local_mcp_bad_request";
    pub const DUPLICATE_REQUEST: &str = "local_mcp_duplicate_request";
    pub const REQUEST_TOO_LARGE: &str = "local_mcp_request_too_large";
    pub const TIMEOUT: &str = "local_mcp_timeout";
    pub const CANCELLED: &str = "local_mcp_cancelled";
    pub const UNREACHABLE: &str = "local_mcp_unreachable";
    pub const HTTP_ERROR: &str = "local_mcp_http_error";
    pub const RESPONSE_TOO_LARGE: &str = "local_mcp_response_too_large";
    pub const MALFORMED_RESPONSE: &str = "local_mcp_malformed_response";
    pub const ID_MISMATCH: &str = "local_mcp_id_mismatch";
    pub const JSONRPC_ERROR: &str = "local_mcp_jsonrpc_error";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMcpConfigError {
    MissingBearerToken,
    MissingContractHash,
    UnsupportedContractEpoch,
    InvalidEndpoint,
    UnsupportedScheme,
    NonLoopbackEndpoint,
    ClientBuildFailed,
}

#[derive(Clone)]
pub struct LocalMcpConfig {
    pub endpoint: String,
    pub bearer_token: String,
    pub contract_hash: String,
    pub contract_epoch: u64,
    pub runtime_version: Option<String>,
    pub runtime_commit: Option<String>,
    pub runtime_generation: Option<String>,
    pub herdr_version: Option<String>,
    pub herdr_protocol: Option<String>,
    pub allow_non_loopback: bool,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub max_frame_bytes: usize,
    pub health_method: String,
    pub health_params: Value,
}

impl LocalMcpConfig {
    pub fn new(bearer_token: impl Into<String>, contract_hash: impl Into<String>) -> Self {
        Self {
            endpoint: LOCAL_MCP_DEFAULT_ENDPOINT.to_owned(),
            bearer_token: bearer_token.into(),
            contract_hash: contract_hash.into(),
            contract_epoch: LOCAL_MCP_CONTRACT_EPOCH,
            runtime_version: None,
            runtime_commit: None,
            runtime_generation: None,
            herdr_version: None,
            herdr_protocol: None,
            allow_non_loopback: false,
            default_timeout_ms: LOCAL_MCP_DEFAULT_TIMEOUT_MS,
            max_timeout_ms: LOCAL_MCP_MAX_TIMEOUT_MS,
            max_frame_bytes: LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES,
            health_method: "server/discover".to_owned(),
            health_params: Value::Object(Map::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeToolResult {
    Success {
        result: Option<Value>,
    },
    Failure {
        code: String,
        retryable: bool,
        message: String,
        details: Option<Value>,
    },
}

impl RuntimeToolResult {
    pub fn failure_code(&self) -> Option<&str> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { code, .. } => Some(code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHealth {
    pub healthy: bool,
    pub details: Option<String>,
}

pub trait LinkRuntimeTransport: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn runtime_info(&self) -> RuntimeContractInfo;
    fn dispatch_request(
        &self,
        request: RuntimeRequest,
    ) -> impl std::future::Future<Output = RuntimeToolResult> + Send;
    fn cancel_request(
        &self,
        request_id: &str,
        reason: &str,
    ) -> impl std::future::Future<Output = ()> + Send;
    fn get_health(&self) -> impl std::future::Future<Output = RuntimeHealth> + Send;
}

#[derive(Clone)]
struct AbortEntry {
    nonce: u64,
    cancel_tx: watch::Sender<bool>,
}

pub struct LocalMcpTransport {
    endpoint: Url,
    bearer_token: String,
    contract_hash: String,
    runtime_version: Option<String>,
    runtime_commit: Option<String>,
    runtime_generation: Option<String>,
    herdr_version: Option<String>,
    herdr_protocol: Option<String>,
    default_timeout_ms: u64,
    max_timeout_ms: u64,
    max_frame_bytes: usize,
    health_method: String,
    health_params: Value,
    client: reqwest::Client,
    discovered_version: RwLock<Option<String>>,
    in_flight: Mutex<BTreeMap<String, AbortEntry>>,
    id_counter: AtomicU64,
    nonce_counter: AtomicU64,
}

impl LocalMcpTransport {
    pub fn new(config: LocalMcpConfig) -> Result<Self, LocalMcpConfigError> {
        if config.bearer_token.is_empty() {
            return Err(LocalMcpConfigError::MissingBearerToken);
        }
        if config.contract_hash.is_empty() {
            return Err(LocalMcpConfigError::MissingContractHash);
        }
        if config.contract_epoch != LOCAL_MCP_CONTRACT_EPOCH {
            return Err(LocalMcpConfigError::UnsupportedContractEpoch);
        }
        let endpoint =
            Url::parse(&config.endpoint).map_err(|_| LocalMcpConfigError::InvalidEndpoint)?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(LocalMcpConfigError::UnsupportedScheme);
        }
        let host = endpoint
            .host_str()
            .ok_or(LocalMcpConfigError::InvalidEndpoint)?;
        if !config.allow_non_loopback && !is_loopback_host(host) {
            return Err(LocalMcpConfigError::NonLoopbackEndpoint);
        }

        // Redirects are disabled deliberately: a loopback endpoint must never
        // forward the bearer credential to another origin.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| LocalMcpConfigError::ClientBuildFailed)?;
        let default_timeout_ms = config.default_timeout_ms.max(1);
        let max_timeout_ms = config.max_timeout_ms.max(1);
        let max_frame_bytes = config
            .max_frame_bytes
            .clamp(LOCAL_MCP_MIN_FRAME_BYTES, LOCAL_MCP_MAX_FRAME_BYTES);
        let health_method = if config.health_method.is_empty() {
            "server/discover".to_owned()
        } else {
            config.health_method
        };

        Ok(Self {
            endpoint,
            bearer_token: config.bearer_token,
            contract_hash: config.contract_hash,
            runtime_version: config.runtime_version,
            runtime_commit: config.runtime_commit,
            runtime_generation: config.runtime_generation,
            herdr_version: config.herdr_version,
            herdr_protocol: config.herdr_protocol,
            default_timeout_ms,
            max_timeout_ms,
            max_frame_bytes,
            health_method,
            health_params: config.health_params,
            client,
            discovered_version: RwLock::new(None),
            in_flight: Mutex::new(BTreeMap::new()),
            id_counter: AtomicU64::new(0),
            nonce_counter: AtomicU64::new(0),
        })
    }

    fn next_rpc_id(&self) -> String {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("local-{id}")
    }

    fn next_nonce(&self) -> u64 {
        self.nonce_counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn lock_in_flight(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, AbortEntry>> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn discovered_version(&self) -> Option<String> {
        self.discovered_version
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn cache_discovered_version(&self, value: String) {
        *self
            .discovered_version
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
    }

    fn runtime_snapshot(&self) -> RuntimeContractInfo {
        RuntimeContractInfo {
            runtime_version: self
                .runtime_version
                .clone()
                .or_else(|| self.discovered_version())
                .unwrap_or_else(|| "unknown".to_owned()),
            runtime_commit: self.runtime_commit.clone(),
            runtime_generation: self.runtime_generation.clone(),
            contract_epoch: Number::from(LOCAL_MCP_CONTRACT_EPOCH),
            contract_hash: Some(self.contract_hash.clone()),
            herdr_version: self.herdr_version.clone(),
            herdr_protocol: self.herdr_protocol.clone(),
        }
    }

    fn failure(
        &self,
        code: &str,
        retryable: bool,
        message: &str,
        details: Option<Value>,
    ) -> RuntimeToolResult {
        RuntimeToolResult::Failure {
            code: code.to_owned(),
            retryable,
            message: message.to_owned(),
            details,
        }
    }

    async fn send_json(&self, body: String) -> Result<reqwest::Response, HttpFailure> {
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.bearer_token))
            .map_err(|_| HttpFailure::Unreachable)?;
        self.client
            .post(self.endpoint.clone())
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(body)
            .send()
            .await
            .map_err(|_| HttpFailure::Unreachable)
    }

    async fn read_bounded_body(
        &self,
        mut response: reqwest::Response,
    ) -> Result<(u16, String), HttpFailure> {
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| HttpFailure::BodyRead)? {
            if bytes.len().saturating_add(chunk.len()) > self.max_frame_bytes {
                return Err(HttpFailure::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn dispatch_http(&self, body: String, rpc_id: String) -> RuntimeToolResult {
        let response = match self.send_json(body).await {
            Ok(response) => response,
            Err(HttpFailure::Unreachable) => {
                return self.failure(code::UNREACHABLE, true, "local runtime unreachable", None);
            }
            Err(_) => unreachable!("send_json only returns unreachable"),
        };
        let (status, text) = match self.read_bounded_body(response).await {
            Ok(value) => value,
            Err(HttpFailure::ResponseTooLarge) => {
                return self.failure(
                    code::RESPONSE_TOO_LARGE,
                    false,
                    "response exceeds maxFrameBytes",
                    Some(json!({"max_bytes": self.max_frame_bytes})),
                );
            }
            Err(HttpFailure::BodyRead) => {
                return self.failure(
                    code::MALFORMED_RESPONSE,
                    false,
                    "malformed MCP response",
                    None,
                );
            }
            Err(HttpFailure::Unreachable) => unreachable!("bounded body has a response"),
        };

        let expected_id = Value::String(rpc_id);
        let parsed = parse_mcp_body(&text, &expected_id);
        if let ParsedBody::RpcError { code: rpc_code } = parsed {
            return self.failure(
                code::JSONRPC_ERROR,
                false,
                "local runtime reported a JSON-RPC error",
                Some(json!({"rpc_code": rpc_code})),
            );
        }
        if !(200..300).contains(&status) {
            return self.failure(
                code::HTTP_ERROR,
                status == 429 || status >= 500,
                "local runtime returned an HTTP error",
                Some(json!({"status": status})),
            );
        }
        match parsed {
            ParsedBody::Result { result } => RuntimeToolResult::Success { result },
            ParsedBody::Malformed => self.failure(
                code::MALFORMED_RESPONSE,
                false,
                "malformed MCP response",
                None,
            ),
            ParsedBody::IdMismatch { .. } => self.failure(
                code::ID_MISMATCH,
                false,
                "MCP response id does not match request",
                None,
            ),
            ParsedBody::RpcError { .. } => unreachable!("rpc error handled above"),
        }
    }

    async fn dispatch_inner(&self, request: RuntimeRequest) -> RuntimeToolResult {
        if request.request_id.is_empty() {
            return self.failure(code::BAD_REQUEST, false, "invalid tool request frame", None);
        }
        if request.operation.is_empty() {
            return self.failure(
                code::BAD_REQUEST,
                false,
                "tool request is missing an operation",
                None,
            );
        }

        let rpc_id = self.next_rpc_id();
        let timeout_hint_ms = request.timeout_hint_ms();
        let body = match serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "tools/call",
            "params": {
                "name": request.operation,
                "arguments": request.arguments.unwrap_or_default(),
            }
        })) {
            Ok(body) => body,
            Err(_) => {
                return self.failure(code::BAD_REQUEST, false, "invalid tool request frame", None);
            }
        };
        if utf8_byte_len(&body) > self.max_frame_bytes {
            return self.failure(
                code::REQUEST_TOO_LARGE,
                false,
                "request exceeds maxFrameBytes",
                Some(json!({"max_bytes": self.max_frame_bytes})),
            );
        }

        let timeout_ms = clamp_request_timeout(
            timeout_hint_ms,
            self.default_timeout_ms,
            self.max_timeout_ms,
        );
        let request_id = request.request_id;
        let nonce = self.next_nonce();
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        {
            let mut in_flight = self.lock_in_flight();
            if in_flight.contains_key(&request_id) {
                return self.failure(
                    code::DUPLICATE_REQUEST,
                    false,
                    "request_id is already in flight",
                    None,
                );
            }
            in_flight.insert(request_id.clone(), AbortEntry { nonce, cancel_tx });
        }

        let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(timeout);
        let result = tokio::select! {
            biased;
            _ = &mut timeout => self.failure(
                code::TIMEOUT,
                true,
                "local runtime request timed out",
                Some(json!({"timeout_ms": timeout_ms})),
            ),
            changed = cancel_rx.changed() => {
                let _ = changed;
                self.failure(
                    code::CANCELLED,
                    false,
                    "local runtime request cancelled",
                    None,
                )
            },
            result = self.dispatch_http(body, rpc_id) => result,
        };

        let mut in_flight = self.lock_in_flight();
        if in_flight
            .get(&request_id)
            .is_some_and(|entry| entry.nonce == nonce)
        {
            in_flight.remove(&request_id);
        }
        result
    }

    async fn cancel_inner(&self, request_id: &str) {
        let sender = self
            .lock_in_flight()
            .get(request_id)
            .map(|entry| entry.cancel_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(true);
        }
    }

    async fn health_inner(&self) -> RuntimeHealth {
        let rpc_id = self.next_rpc_id();
        let body = match serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": self.health_method,
            "params": self.health_params,
        })) {
            Ok(body) => body,
            Err(_) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some("malformed".to_owned()),
                };
            }
        };
        let timeout_ms = clamp_request_timeout(None, self.default_timeout_ms, self.max_timeout_ms);
        let outcome = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
            let response = self.send_json(body).await?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Ok::<_, HttpFailure>(HealthHttp::Http(status));
            }
            let (_, text) = self.read_bounded_body(response).await?;
            Ok(HealthHttp::Body(text))
        })
        .await;

        let text = match outcome {
            Err(_) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some("timeout".to_owned()),
                };
            }
            Ok(Err(HttpFailure::Unreachable)) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some("unreachable".to_owned()),
                };
            }
            Ok(Err(HttpFailure::BodyRead)) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some("malformed".to_owned()),
                };
            }
            Ok(Err(HttpFailure::ResponseTooLarge)) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some("response_too_large".to_owned()),
                };
            }
            Ok(Ok(HealthHttp::Http(status))) => {
                return RuntimeHealth {
                    healthy: false,
                    details: Some(format!("http_{status}")),
                };
            }
            Ok(Ok(HealthHttp::Body(text))) => text,
        };

        match parse_mcp_body(&text, &Value::String(rpc_id)) {
            ParsedBody::Malformed => RuntimeHealth {
                healthy: false,
                details: Some("malformed".to_owned()),
            },
            ParsedBody::IdMismatch { .. } => RuntimeHealth {
                healthy: false,
                details: Some("id_mismatch".to_owned()),
            },
            ParsedBody::RpcError { .. } => RuntimeHealth {
                healthy: false,
                details: Some("rpc_error".to_owned()),
            },
            ParsedBody::Result { result } => {
                if let Some(version) = result.as_ref().and_then(find_server_version) {
                    self.cache_discovered_version(version.to_owned());
                }
                RuntimeHealth {
                    healthy: true,
                    details: None,
                }
            }
        }
    }
}

impl LinkRuntimeTransport for LocalMcpTransport {
    fn name(&self) -> &str {
        "local-mcp-http"
    }

    fn runtime_info(&self) -> RuntimeContractInfo {
        self.runtime_snapshot()
    }

    async fn dispatch_request(&self, request: RuntimeRequest) -> RuntimeToolResult {
        self.dispatch_inner(request).await
    }

    async fn cancel_request(&self, request_id: &str, reason: &str) {
        let _ = reason;
        self.cancel_inner(request_id).await;
    }

    async fn get_health(&self) -> RuntimeHealth {
        self.health_inner().await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpFailure {
    Unreachable,
    BodyRead,
    ResponseTooLarge,
}

enum HealthHttp {
    Http(u16),
    Body(String),
}

#[derive(Debug, Clone, PartialEq)]
enum ParsedBody {
    Result { result: Option<Value> },
    RpcError { code: Option<i64> },
    IdMismatch { parsed: usize },
    Malformed,
}

#[derive(Debug, Clone, PartialEq)]
enum ScanOutcome {
    Messages(Vec<Value>),
    Malformed,
}

pub fn is_loopback_host(hostname: &str) -> bool {
    let host = hostname
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(hostname)
        .to_ascii_lowercase();
    if host == "localhost"
        || host == "::1"
        || host == "0:0:0:0:0:0:0:1"
        || host == "0:0:0:0:0:0:0:0:1"
    {
        return true;
    }
    let octets = host.split('.').collect::<Vec<_>>();
    if octets.len() != 4 || octets[0] != "127" {
        return false;
    }
    octets
        .iter()
        .all(|octet| !octet.is_empty() && octet.len() <= 3 && octet.parse::<u8>().is_ok())
}

pub fn utf8_byte_len(text: &str) -> usize {
    text.len()
}

pub fn clamp_request_timeout(hint: Option<f64>, fallback: u64, max: u64) -> u64 {
    let base = match hint {
        Some(value) if value.is_finite() && value >= 1.0 => value.floor() as u64,
        _ => fallback.max(1),
    };
    base.max(1).min(max.max(1))
}

fn parse_mcp_body(text: &str, expected_id: &Value) -> ParsedBody {
    let messages = match scan_rpc_messages(text) {
        ScanOutcome::Messages(messages) => messages,
        ScanOutcome::Malformed => return ParsedBody::Malformed,
    };
    let rpc = messages
        .into_iter()
        .filter(|message| {
            message.as_object().and_then(|object| object.get("jsonrpc"))
                == Some(&Value::String("2.0".to_owned()))
        })
        .collect::<Vec<_>>();
    if rpc.is_empty() {
        return ParsedBody::Malformed;
    }
    let Some(hit) = rpc.iter().find(|message| {
        message.as_object().and_then(|object| object.get("id")) == Some(expected_id)
    }) else {
        return ParsedBody::IdMismatch { parsed: rpc.len() };
    };
    let object = hit
        .as_object()
        .expect("RPC messages were filtered as objects");
    if let Some(error) = object.get("error") {
        let code = error
            .as_object()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_i64);
        return ParsedBody::RpcError { code };
    }
    ParsedBody::Result {
        result: object.get("result").cloned(),
    }
}

fn scan_rpc_messages(text: &str) -> ScanOutcome {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ScanOutcome::Messages(Vec::new());
    }
    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(trimmed)
    {
        return ScanOutcome::Messages(vec![value]);
    }

    let mut messages = Vec::new();
    let mut data_lines = Vec::new();
    let flush = |data_lines: &mut Vec<String>, messages: &mut Vec<Value>| -> bool {
        if data_lines.is_empty() {
            return true;
        }
        let joined = data_lines.join("\n");
        data_lines.clear();
        match serde_json::from_str::<Value>(&joined) {
            Ok(value) => {
                messages.push(value);
                true
            }
            Err(_) => false,
        }
    };

    for raw_line in text.lines() {
        if raw_line.is_empty() {
            if !flush(&mut data_lines, &mut messages) {
                return ScanOutcome::Malformed;
            }
            continue;
        }
        if raw_line.starts_with(':') {
            continue;
        }
        if let Some(data) = raw_line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            continue;
        }
        if raw_line.starts_with("event:")
            || raw_line.starts_with("id:")
            || raw_line.starts_with("retry:")
        {
            continue;
        }
    }
    if !flush(&mut data_lines, &mut messages) {
        return ScanOutcome::Malformed;
    }
    ScanOutcome::Messages(messages)
}

fn find_server_version(result: &Value) -> Option<&str> {
    let object = result.as_object()?;
    if let Some(version) = object
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
        .and_then(Value::as_object)
        .and_then(|server_info| server_info.get("version"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Some(version);
    }
    for path in [["serverInfo", "version"], ["server_info", "version"]] {
        if let Some(version) = object
            .get(path[0])
            .and_then(Value::as_object)
            .and_then(|nested| nested.get(path[1]))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Some(version);
        }
    }
    object
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Number, Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::{
        LOCAL_MCP_CONTRACT_EPOCH, LinkRuntimeTransport, LocalMcpConfig, LocalMcpConfigError,
        LocalMcpTransport, ParsedBody, RuntimeToolResult, ScanOutcome, clamp_request_timeout, code,
        is_loopback_host, parse_mcp_body, scan_rpc_messages, utf8_byte_len,
    };
    use crate::link::request_core::RuntimeRequest;

    fn request(id: &str) -> RuntimeRequest {
        RuntimeRequest {
            workstation_id: "ws1".to_owned(),
            request_id: id.to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: Some(serde_json::Map::from_iter([(
                "query".to_owned(),
                Value::String("ping".to_owned()),
            )])),
            timeout_ms: Some(Number::from(500)),
            contract_epoch: Some(Number::from(2)),
            contract_hash: Some("sha256:test".to_owned()),
            idempotency_key: None,
            trace: None,
        }
    }

    async fn spawn_server(
        status: u16,
        body: String,
        delay_ms: u64,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let _ = request_tx.send(String::from_utf8_lossy(&bytes).into_owned());
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "ERR"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        (format!("http://{address}/mcp"), request_rx)
    }

    async fn spawn_disconnect_observer() -> (String, oneshot::Receiver<()>, oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = oneshot::channel();
        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..read]);
                let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let _ = request_tx.send(());
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => {
                        let _ = disconnect_tx.send(());
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
        (format!("http://{address}/mcp"), request_rx, disconnect_rx)
    }

    fn transport_for(endpoint: String) -> LocalMcpTransport {
        let mut config = LocalMcpConfig::new("token-test", "sha256:test");
        config.endpoint = endpoint;
        LocalMcpTransport::new(config).unwrap()
    }

    #[test]
    fn pure_helpers_match_node_oracle() {
        for host in [
            "127.0.0.1",
            "127.250.1.9",
            "localhost",
            "[::1]",
            "::1",
            "0:0:0:0:0:0:0:1",
            "0:0:0:0:0:0:0:0:1",
        ] {
            assert!(is_loopback_host(host), "expected loopback {host}");
        }
        for host in ["127.0.0.256", "192.168.1.5", "10.0.0.1", "herdr.local"] {
            assert!(!is_loopback_host(host), "expected non-loopback {host}");
        }
        assert_eq!(utf8_byte_len("é"), 2);
        assert_eq!(clamp_request_timeout(None, 10_000, 120_000), 10_000);
        assert_eq!(clamp_request_timeout(Some(9.9), 10_000, 120_000), 9);
        assert_eq!(clamp_request_timeout(Some(999.0), 10_000, 30), 30);
    }

    #[test]
    fn parser_accepts_plain_json_and_sse_with_exact_id() {
        assert_eq!(
            parse_mcp_body(
                r#"{"jsonrpc":"2.0","id":"local-1","result":{"ok":true}}"#,
                &Value::String("local-1".to_owned())
            ),
            ParsedBody::Result {
                result: Some(json!({"ok": true}))
            }
        );
        let sse = concat!(
            ": keepalive\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"notifications/progress\"}\n\n",
            "id: 2\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":\"local-2\",\"result\":{\"ok\":2}}\n\n"
        );
        assert_eq!(
            parse_mcp_body(sse, &Value::String("local-2".to_owned())),
            ParsedBody::Result {
                result: Some(json!({"ok": 2}))
            }
        );
        assert!(matches!(
            parse_mcp_body(
                r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                &Value::String("1".to_owned())
            ),
            ParsedBody::IdMismatch { parsed: 1 }
        ));
        assert_eq!(scan_rpc_messages("data: {bad}\n\n"), ScanOutcome::Malformed);
    }

    #[test]
    fn construction_is_loopback_epoch_and_secret_fail_closed() {
        assert_eq!(
            LocalMcpTransport::new(LocalMcpConfig::new("", "sha256:test"))
                .err()
                .unwrap(),
            LocalMcpConfigError::MissingBearerToken
        );
        assert_eq!(
            LocalMcpTransport::new(LocalMcpConfig::new("token", ""))
                .err()
                .unwrap(),
            LocalMcpConfigError::MissingContractHash
        );
        let mut epoch = LocalMcpConfig::new("token", "sha256:test");
        epoch.contract_epoch = LOCAL_MCP_CONTRACT_EPOCH - 1;
        assert_eq!(
            LocalMcpTransport::new(epoch).err().unwrap(),
            LocalMcpConfigError::UnsupportedContractEpoch
        );
        let mut remote = LocalMcpConfig::new("token", "sha256:test");
        remote.endpoint = "http://192.168.1.5:8772/mcp".to_owned();
        assert_eq!(
            LocalMcpTransport::new(remote).err().unwrap(),
            LocalMcpConfigError::NonLoopbackEndpoint
        );
        let mut insecure_shape = LocalMcpConfig::new("token", "sha256:test");
        insecure_shape.endpoint = "ws://127.0.0.1:8772/mcp".to_owned();
        assert_eq!(
            LocalMcpTransport::new(insecure_shape).err().unwrap(),
            LocalMcpConfigError::UnsupportedScheme
        );
    }

    #[tokio::test]
    async fn dispatch_posts_exact_tools_call_and_preserves_full_result_envelope() {
        let result = json!({
            "content": [{"type": "image", "data": "abc", "mimeType": "image/png"}],
            "structuredContent": {"n": 1},
            "_meta": {"trace": "safe"},
            "isError": true
        });
        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": result});
        let (endpoint, request_rx) = spawn_server(200, response.to_string(), 0).await;
        let transport = transport_for(endpoint);
        let outcome = transport.dispatch_request(request("r1")).await;
        assert_eq!(
            outcome,
            RuntimeToolResult::Success {
                result: Some(result.clone())
            }
        );
        let raw_request = request_rx.await.unwrap();
        assert!(raw_request.starts_with("POST /mcp HTTP/1.1"));
        assert!(
            raw_request
                .to_ascii_lowercase()
                .contains("authorization: bearer token-test")
        );
        let body = raw_request.split("\r\n\r\n").nth(1).unwrap();
        let value: Value = serde_json::from_str(body).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "local-1");
        assert_eq!(value["method"], "tools/call");
        assert_eq!(value["params"]["name"], "herdr_inspect");
        assert_eq!(value["params"]["arguments"], json!({"query": "ping"}));
    }

    #[tokio::test]
    async fn jsonrpc_and_http_failures_are_sanitized() {
        let token = "token-never-returned";
        let response = json!({
            "jsonrpc": "2.0",
            "id": "local-1",
            "error": {"code": -32602, "message": format!("echo {token}"), "data": {"raw": token}}
        });
        let (endpoint, _) = spawn_server(500, response.to_string(), 0).await;
        let mut config = LocalMcpConfig::new(token, "sha256:test");
        config.endpoint = endpoint;
        let transport = LocalMcpTransport::new(config).unwrap();
        let outcome = transport.dispatch_request(request("r1")).await;
        match &outcome {
            RuntimeToolResult::Failure {
                code: got,
                retryable,
                details,
                ..
            } => {
                assert_eq!(got, code::JSONRPC_ERROR);
                assert!(!retryable);
                assert_eq!(details, &Some(json!({"rpc_code": -32602})));
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(!format!("{outcome:?}").contains(token));

        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": {}});
        let (endpoint, _) = spawn_server(503, response.to_string(), 0).await;
        let outcome = transport_for(endpoint)
            .dispatch_request(request("r2"))
            .await;
        assert!(matches!(
            outcome,
            RuntimeToolResult::Failure { ref code, retryable: true, .. } if code == super::code::HTTP_ERROR
        ));
    }

    #[tokio::test]
    async fn timeout_and_cancel_are_request_scoped() {
        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": {}});
        let (endpoint, _) = spawn_server(200, response.to_string(), 200).await;
        let mut config = LocalMcpConfig::new("token", "sha256:test");
        config.endpoint = endpoint;
        config.default_timeout_ms = 20;
        config.max_timeout_ms = 20;
        let transport = LocalMcpTransport::new(config).unwrap();
        let mut req = request("timeout");
        req.timeout_ms = None;
        let outcome = transport.dispatch_request(req).await;
        assert!(matches!(
            outcome,
            RuntimeToolResult::Failure { ref code, retryable: true, .. } if code == super::code::TIMEOUT
        ));

        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": {}});
        let (endpoint, _) = spawn_server(200, response.to_string(), 200).await;
        let transport = Arc::new(transport_for(endpoint));
        let dispatch_transport = Arc::clone(&transport);
        let task =
            tokio::spawn(
                async move { dispatch_transport.dispatch_request(request("cancel")).await },
            );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        transport.cancel_request("cancel", "user cancelled").await;
        let outcome = task.await.unwrap();
        assert!(matches!(
            outcome,
            RuntimeToolResult::Failure { ref code, retryable: false, .. } if code == super::code::CANCELLED
        ));
        transport.cancel_request("cancel", "again").await;
    }

    #[tokio::test]
    async fn cancel_drops_the_inflight_http_future_and_closes_the_socket() {
        let (endpoint, request_rx, disconnect_rx) = spawn_disconnect_observer().await;
        let transport = Arc::new(transport_for(endpoint));
        let dispatch_transport = Arc::clone(&transport);
        let task = tokio::spawn(async move {
            dispatch_transport
                .dispatch_request(request("cancel-http"))
                .await
        });
        request_rx.await.unwrap();
        transport.cancel_request("cancel-http", "edge_cancel").await;
        let outcome = task.await.unwrap();
        assert!(matches!(
            outcome,
            RuntimeToolResult::Failure { ref code, retryable: false, .. }
                if code == super::code::CANCELLED
        ));
        tokio::time::timeout(std::time::Duration::from_millis(250), disconnect_rx)
            .await
            .expect("dropping the reqwest future should close the in-flight HTTP socket")
            .unwrap();
    }

    #[tokio::test]
    async fn duplicate_and_unrelated_cancel_do_not_disturb_the_original_dispatch() {
        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": {"ok": true}});
        let (endpoint, _) = spawn_server(200, response.to_string(), 80).await;
        let transport = Arc::new(transport_for(endpoint));
        let dispatch_transport = Arc::clone(&transport);
        let task = tokio::spawn(async move {
            dispatch_transport
                .dispatch_request(request("original"))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let duplicate = transport.dispatch_request(request("original")).await;
        assert!(matches!(
            duplicate,
            RuntimeToolResult::Failure { ref code, retryable: false, .. }
                if code == super::code::DUPLICATE_REQUEST
        ));
        transport.cancel_request("unrelated", "no-op").await;

        assert_eq!(
            task.await.unwrap(),
            RuntimeToolResult::Success {
                result: Some(json!({"ok": true}))
            }
        );
    }

    #[tokio::test]
    async fn response_budget_and_health_version_cache_are_bounded() {
        let large = "x".repeat(600);
        let response = json!({"jsonrpc": "2.0", "id": "local-1", "result": {"large": large}});
        let (endpoint, _) = spawn_server(200, response.to_string(), 0).await;
        let mut config = LocalMcpConfig::new("token", "sha256:test");
        config.endpoint = endpoint;
        config.max_frame_bytes = 256;
        let transport = LocalMcpTransport::new(config).unwrap();
        let outcome = transport.dispatch_request(request("r1")).await;
        assert!(matches!(
            outcome,
            RuntimeToolResult::Failure { ref code, retryable: false, .. } if code == super::code::RESPONSE_TOO_LARGE
        ));

        let response = json!({
            "jsonrpc": "2.0",
            "id": "local-1",
            "result": {
                "_meta": {"io.modelcontextprotocol/serverInfo": {"version": "0.4.0-test"}}
            }
        });
        let (endpoint, _) = spawn_server(200, response.to_string(), 0).await;
        let transport = transport_for(endpoint);
        assert!(transport.get_health().await.healthy);
        assert_eq!(transport.runtime_info().runtime_version, "0.4.0-test");
    }
}
