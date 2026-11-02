//! Chrome Native Messaging data plane for the Rust runtime.
//!
//! The installer remains on the existing compatibility path for now. This
//! module implements the current extension's request/stream protocol over the
//! mode-0600 Unix socket so the browser data plane no longer needs Node once
//! the Rust runtime is activated in production.

use serde_json::{Value, json};
use std::io::{Read, Write};
#[cfg(unix)]
use std::process::Command;
use std::process::ExitCode;
use std::time::Instant;

use crate::native_host_log::NativeHostLog;

#[cfg(unix)]
use serde_json::Map;
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use url::Url;

#[cfg(unix)]
use base64::Engine as _;
#[cfg(unix)]
use base64::engine::general_purpose::STANDARD as BASE64;
#[cfg(unix)]
use futures_util::stream::{self, StreamExt};
#[cfg(unix)]
use http_body_util::{BodyExt, Full};
#[cfg(unix)]
use hyper::body::{Bytes, Incoming};
#[cfg(unix)]
use hyper::client::conn::http1;
#[cfg(unix)]
use hyper::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, HOST, HeaderName, HeaderValue};
#[cfg(unix)]
use hyper::{Method, Request, Response};
#[cfg(unix)]
use hyper_util::rt::TokioIo;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tokio::time::{MissedTickBehavior, interval, timeout};

const MAX_NATIVE_MESSAGE: usize = 1024 * 1024;
/// Largest frame the native host will read from the browser. Generic messages
/// stay capped at [`MAX_NATIVE_MESSAGE`]; only the dedicated `artifact_capture`
/// message is permitted to exceed that within this bounded envelope so it can
/// carry up to 8 MiB of raw artifact bytes plus base64 JSON.
const MAX_NATIVE_MESSAGE_FRAME: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8772";
#[cfg(unix)]
const MAX_REQUEST_BODY: usize = 1024 * 1024;
#[cfg(unix)]
const MAX_RESPONSE_BODY: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const STREAM_CHUNK_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
#[cfg(unix)]
const MIN_TIMEOUT_MS: u64 = 1_000;
#[cfg(unix)]
const MAX_TIMEOUT_MS: u64 = 120_000;
#[cfg(unix)]
const MAX_NATIVE_REQUEST_BATCH: usize = 24;
#[cfg(unix)]
const MAX_NATIVE_REQUEST_BATCH_PARALLEL: usize = 4;

#[cfg(unix)]
const ALLOWED_PROXY_PATHS: &[&str] = &[
    "/mcp",
    "/push/state",
    "/push/events",
    "/push/mcp-activity",
    "/extension/control/action",
    "/extension/fleet",
    "/extension/browser/registry",
    "/extension/browser/actuation",
    "/extension/semantic/status",
    "/extension/semantic/evaluate",
    "/extension/semantic/chat",
    "/extension/continuity/turn",
    "/extension/continuity/resolve",
];
#[cfg(unix)]
const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "mcp-protocol-version",
    "mcp-session-id",
    "x-herdr-client",
];

#[cfg(unix)]
#[derive(Debug, Clone)]
struct HostConfig {
    expected_origin: String,
    socket_path: PathBuf,
    browser_family: String,
    #[cfg(test)]
    enforce_owner_fence: bool,
}

#[cfg(unix)]
impl HostConfig {
    fn from_env() -> Result<Self, String> {
        let expected_origin = expected_extension_origin()?;
        let socket_path = env::var_os("HERDR_EXTENSION_IPC_SOCKET")
            .map(PathBuf::from)
            .unwrap_or(default_socket_path()?);
        Ok(Self {
            expected_origin,
            socket_path,
            browser_family: detect_parent_browser_family(),
            #[cfg(test)]
            enforce_owner_fence: true,
        })
    }
}

#[cfg(unix)]
fn classify_browser_family(command: &str) -> &'static str {
    let command = command.to_ascii_lowercase();
    if command.contains("ego lite") || command.contains("/ego.app/") {
        "ego"
    } else if command.contains("microsoft edge") || command.contains("msedge") {
        "edge"
    } else if command.contains("brave browser") || command.contains("brave-browser") {
        "brave"
    } else if command.contains("google chrome") || command.contains("chrome for testing") {
        "chrome"
    } else {
        "chromium"
    }
}

#[cfg(unix)]
fn detect_parent_browser_family() -> String {
    // Chromium launches a Native Messaging host as a direct child of the
    // browser process. Trust that process boundary instead of page JS/UA or a
    // browser-family value supplied by the extension.
    let parent_pid = unsafe { libc::getppid() };
    if parent_pid <= 1 {
        return "chromium".to_owned();
    }
    let parent_pid = parent_pid.to_string();
    let command = Command::new("ps")
        .args(["-p", parent_pid.as_str(), "-o", "command="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    classify_browser_family(&command).to_owned()
}

#[cfg(unix)]
fn owner_is_active(expected_origin: &str, registered_origin: Option<&str>) -> bool {
    registered_origin == Some(expected_origin)
}

#[cfg(unix)]
fn require_active_owner(config: &HostConfig) -> Result<(), String> {
    #[cfg(test)]
    if !config.enforce_owner_fence {
        return Ok(());
    }
    let registered = crate::native_host_install::current_registered_extension_origin()?;
    if !owner_is_active(&config.expected_origin, registered.as_deref()) {
        return Err("extension_origin_not_active".to_owned());
    }
    Ok(())
}

/// Error codes this module is allowed to write to disk. Anything else — an OS
/// error tail, a path, or a payload-derived string — collapses to `host_error`,
/// so a future refactor cannot leak content into the log through an error
/// string. `completed` and `stream_end` are the success disconnect reasons.
const NATIVE_HOST_LOG_CODES: &[&str] = &[
    "completed",
    "stream_end",
    "host_error",
    "invalid_native_message",
    "native_message_too_large",
    "unsupported_message",
    "unsupported_platform",
    "legacy_session_requires_compat_host",
    "extension_origin_not_allowed",
    "extension_origin_unconfigured",
    "extension_origin_not_active",
    "extension_ipc_unavailable",
    "native_request_timeout",
    "native_request_too_large",
    "native_request_invalid",
    "native_request_batch_invalid",
    "native_request_batch_empty",
    "native_request_batch_too_large",
    "native_response_too_large",
    "native_response_failed",
    "native_response_encode_failed",
    "native_stdout_failed",
    "native_stream_connect_timeout",
    "native_stream_failed",
    "stream_path_not_allowed",
    "proxy_path_not_allowed",
    "proxy_method_not_allowed",
    "invalid_proxy_header",
    "base_url_must_be_loopback_http",
    "native_browser_registry_body_invalid",
    "artifact_capture_invalid",
    "artifact_capture_unknown_field",
    "artifact_capture_invalid_base64",
    "artifact_capture_invalid_sha256",
    "artifact_capture_too_large",
];

/// Message types this module is allowed to name in the log. Anything else is
/// recorded as `other`, so a message body can never reach the log through a
/// `type` value.
const NATIVE_HOST_MESSAGE_TYPES: &[&str] = &[
    "identity",
    "request",
    "request_batch",
    "stream",
    "session",
    "artifact_capture",
];

fn log_code(code: &str) -> &'static str {
    let kind = code.split(':').next().unwrap_or_default().trim();
    NATIVE_HOST_LOG_CODES
        .iter()
        .copied()
        .find(|known| *known == kind)
        .unwrap_or("host_error")
}

fn message_type_code(message: &Value) -> &'static str {
    let raw = message.get("type").and_then(Value::as_str).unwrap_or("");
    NATIVE_HOST_MESSAGE_TYPES
        .iter()
        .copied()
        .find(|known| *known == raw)
        .unwrap_or("other")
}

/// Only a syntactically valid Chromium extension origin is written; anything
/// else (an empty argument, a page origin, a stray credential) is recorded as
/// `-` rather than copied to disk.
fn log_origin(origin: &str) -> String {
    #[cfg(unix)]
    {
        if validate_extension_origin(origin).is_ok() {
            return origin.to_owned();
        }
    }
    #[cfg(not(unix))]
    let _ = origin;
    "-".to_owned()
}

fn record_connect(
    log: Option<&NativeHostLog>,
    caller_origin: &str,
    read: &Result<(Value, usize), String>,
) {
    let Some(log) = log else {
        return;
    };
    let mut fields = vec![
        ("version", env!("CARGO_PKG_VERSION").to_owned()),
        ("pid", std::process::id().to_string()),
        ("extension_origin", log_origin(caller_origin)),
    ];
    match read {
        Ok((message, encoded_size)) => {
            fields.push(("message_type", message_type_code(message).to_owned()));
            fields.push(("encoded_bytes", encoded_size.to_string()));
        }
        Err(_) => fields.push(("message_type", "-".to_owned())),
    }
    log.record("connect", &fields);
}

fn record_disconnect(
    log: Option<&NativeHostLog>,
    reason: &str,
    message_type: &str,
    started: Instant,
) {
    let Some(log) = log else {
        return;
    };
    log.record(
        "disconnect",
        &[
            ("reason", log_code(reason).to_owned()),
            ("message_type", message_type.to_owned()),
            ("duration_ms", started.elapsed().as_millis().to_string()),
        ],
    );
}

/// A port/IO failure is recorded next to the connect/disconnect pair so a log
/// reader can tell "the socket was gone" from "the extension never polled".
fn record_io_error(log: Option<&NativeHostLog>, code: &str) {
    let Some(log) = log else {
        return;
    };
    log.record("io_error", &[("code", log_code(code).to_owned())]);
}

struct HostOutcome {
    reason: &'static str,
    error: Option<String>,
}

impl HostOutcome {
    fn completed(reason: &'static str) -> Self {
        Self {
            reason,
            error: None,
        }
    }

    fn failed(reason: &'static str, error: String) -> Self {
        Self {
            reason,
            error: Some(error),
        }
    }
}

/// Answer the browser with an error frame and keep the host alive. Only a failed
/// stdout write is fatal, because the browser would otherwise wait forever.
fn refuse<W: Write>(writer: &mut W, code: &str) -> HostOutcome {
    match write_error(writer, code) {
        Ok(()) => HostOutcome::completed(log_code(code)),
        Err(error) => HostOutcome::failed("native_stdout_failed", error),
    }
}

fn finish(write: Result<(), String>, reason: &'static str) -> HostOutcome {
    match write {
        Ok(()) => HostOutcome::completed(reason),
        Err(error) => HostOutcome::failed("native_stdout_failed", error),
    }
}

pub fn run(caller_origin: &str) -> Result<ExitCode, String> {
    let log = NativeHostLog::discover();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    #[cfg(unix)]
    {
        serve(
            caller_origin,
            &mut reader,
            &mut writer,
            log.as_ref(),
            HostConfig::from_env,
        )
    }
    #[cfg(not(unix))]
    {
        serve(caller_origin, &mut reader, &mut writer, log.as_ref())
    }
}

/// One native-host process lifetime: read one framed message, serve it, and
/// record the content-free connect/disconnect pair around it.
///
/// `resolve_config` is the production environment read. Tests inject a config
/// because they cannot own the installed extension registry.
fn serve<R: Read, W: Write>(
    caller_origin: &str,
    reader: &mut R,
    writer: &mut W,
    log: Option<&NativeHostLog>,
    #[cfg(unix)] resolve_config: impl FnOnce() -> Result<HostConfig, String>,
) -> Result<ExitCode, String> {
    let started = Instant::now();
    let read = read_native_message(reader);
    record_connect(log, caller_origin, &read);
    let message_type = match &read {
        Ok((message, _)) => message_type_code(message),
        Err(_) => "-",
    };
    let outcome = serve_message(
        caller_origin,
        read,
        writer,
        log,
        #[cfg(unix)]
        resolve_config,
    );
    record_disconnect(log, outcome.reason, message_type, started);
    match outcome.error {
        Some(error) => Err(error),
        None => Ok(ExitCode::SUCCESS),
    }
}

fn serve_message<W: Write>(
    caller_origin: &str,
    read: Result<(Value, usize), String>,
    writer: &mut W,
    log: Option<&NativeHostLog>,
    #[cfg(unix)] resolve_config: impl FnOnce() -> Result<HostConfig, String>,
) -> HostOutcome {
    let (message, encoded_size) = match read {
        Ok(parsed) => parsed,
        Err(error) => return refuse(writer, &error),
    };

    #[cfg(not(unix))]
    {
        let _ = caller_origin;
        let _ = encoded_size;
        let _ = log;
        return refuse(writer, "unsupported_platform");
    }

    #[cfg(unix)]
    {
        let config = match resolve_config() {
            Ok(config) => config,
            Err(error) => return refuse(writer, &error),
        };
        let effective_caller_origin = if caller_origin.is_empty() {
            config.expected_origin.as_str()
        } else {
            caller_origin
        };
        if effective_caller_origin != config.expected_origin {
            return refuse(writer, "extension_origin_not_allowed");
        }

        let message_type = message.get("type").and_then(Value::as_str).unwrap_or("");
        // Only the dedicated artifact capture message may exceed the generic
        // native-message bound. All other messages remain capped at 1 MiB
        // measured against the bytes the host actually read.
        if let Some(error) = enforce_message_size_gate(message_type, encoded_size) {
            return refuse(writer, &error);
        }

        if message_type == "identity" {
            let active = require_active_owner(&config).is_ok();
            return finish(
                write_native_message(
                    writer,
                    &json!({
                        "ok": true,
                        "active": active,
                        "extension_origin": config.expected_origin,
                        "browser_family": config.browser_family,
                        "transport": "native",
                    }),
                ),
                "completed",
            );
        }

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                return HostOutcome::failed(
                    "host_error",
                    format!("cannot build native-host runtime: {error}"),
                );
            }
        };
        match message_type {
            "request" => match runtime.block_on(proxy_request(&config, &message)) {
                Ok(value) => finish(write_native_message(writer, &value), "completed"),
                Err(error) => {
                    record_io_error(log, &error);
                    refuse(writer, &error)
                }
            },
            "request_batch" => match runtime.block_on(proxy_request_batch(&config, &message)) {
                Ok(value) => finish(write_native_message(writer, &value), "completed"),
                Err(error) => {
                    record_io_error(log, &error);
                    refuse(writer, &error)
                }
            },
            "stream" => match runtime.block_on(proxy_stream(&config, &message, writer)) {
                Ok(()) => HostOutcome::completed("stream_end"),
                Err(error) => {
                    record_io_error(log, &error);
                    refuse(writer, &error)
                }
            },
            "session" => refuse(writer, "legacy_session_requires_compat_host"),
            "artifact_capture" => match handle_artifact_capture(&config, &message) {
                Ok(value) => finish(write_native_message(writer, &value), "completed"),
                Err(error) => refuse(writer, &error),
            },
            _ => refuse(writer, "unsupported_message"),
        }
    }
}

/// Per-type size gate: only `artifact_capture` may exceed [`MAX_NATIVE_MESSAGE`].
/// All other message types stay capped at exactly 1 MiB measured against the
/// bytes the host actually read from the browser.
#[cfg(unix)]
fn enforce_message_size_gate(message_type: &str, encoded_size: usize) -> Option<String> {
    if message_type != "artifact_capture" && encoded_size > MAX_NATIVE_MESSAGE {
        Some("native_message_too_large".to_owned())
    } else {
        None
    }
}

#[cfg(unix)]
fn default_socket_path() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory".to_owned())?;
    Ok(home
        .join(".config")
        .join("herdr-mcp")
        .join("extension.sock"))
}

#[cfg(unix)]
fn expected_extension_origin() -> Result<String, String> {
    if let Ok(origin) = env::var("HERDR_EXTENSION_ORIGIN")
        && !origin.trim().is_empty()
    {
        return validate_extension_origin(origin.trim()).map(str::to_owned);
    }

    if let Some(path) = extension_path_for_install_optional()? {
        let id = chromium_id_for_path(&path)?;
        return Ok(format!("chrome-extension://{id}/"));
    }
    Ok(crate::browser_extension_identity::official_store_identity()?.origin)
}

#[cfg(unix)]
pub(crate) fn extension_path_for_install() -> Result<PathBuf, String> {
    extension_path_for_install_optional()?.ok_or_else(|| {
        "extension directory not found: set HERDR_EXTENSION_PATH to an unpacked development extension directory when using an unpacked build"
            .to_owned()
    })
}

#[cfg(unix)]
pub(crate) fn extension_path_for_install_optional() -> Result<Option<PathBuf>, String> {
    // Production is Store-first. An unpacked identity is selected only by an
    // explicit maintainer override; legacy managed/check-out extension paths
    // must never silently change the Native Messaging origin.
    let Some(raw) = env::var_os("HERDR_EXTENSION_PATH") else {
        return Ok(None);
    };
    let path = lexical_absolute(&PathBuf::from(raw))?;
    require_extension_dir(
        &path,
        "HERDR_EXTENSION_PATH points to a missing or incomplete unpacked development extension directory",
    )
    .map(Some)
}

#[cfg(unix)]
fn is_extension_dir(path: &Path) -> bool {
    path.is_dir() && path.join("manifest.json").is_file()
}

#[cfg(unix)]
pub(crate) fn require_extension_dir(path: &Path, context: &str) -> Result<PathBuf, String> {
    if is_extension_dir(path) {
        Ok(path.to_path_buf())
    } else {
        Err(format!(
            "{context}: expected a directory containing manifest.json at {}",
            path.display()
        ))
    }
}

#[cfg(unix)]
pub(crate) fn validate_extension_origin(origin: &str) -> Result<&str, String> {
    let Some(id) = origin
        .strip_prefix("chrome-extension://")
        .and_then(|value| value.strip_suffix('/'))
    else {
        return Err("extension_origin_unconfigured".to_owned());
    };
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return Err("extension_origin_unconfigured".to_owned());
    }
    Ok(origin)
}

#[cfg(unix)]
pub(crate) fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| format!("cannot resolve extension path: {error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(unix)]
pub(crate) fn chromium_id_for_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "extension_path_not_utf8".to_owned())?;
    let digest = Sha256::digest(text.as_bytes());
    let mut id = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        id.push(char::from(b'a' + (byte >> 4)));
        id.push(char::from(b'a' + (byte & 0x0f)));
    }
    Ok(id)
}

fn read_native_message<R: Read>(reader: &mut R) -> Result<(Value, usize), String> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| "invalid_native_message".to_owned())?;
    let size = u32::from_le_bytes(header) as usize;
    if size > MAX_NATIVE_MESSAGE_FRAME {
        return Err("native_message_too_large".to_owned());
    }
    let mut body = vec![0_u8; size];
    reader
        .read_exact(&mut body)
        .map_err(|_| "invalid_native_message".to_owned())?;
    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => Ok((value, size)),
        Err(_) => Err("invalid_native_message".to_owned()),
    }
}

fn write_native_message<W: Write>(writer: &mut W, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|_| "native_response_encode_failed".to_owned())?;
    let size = u32::try_from(body.len()).map_err(|_| "native_response_too_large".to_owned())?;
    writer
        .write_all(&size.to_le_bytes())
        .and_then(|_| writer.write_all(&body))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("native_stdout_failed: {error}"))
}

fn write_error<W: Write>(writer: &mut W, error: &str) -> Result<(), String> {
    write_native_message(writer, &json!({"ok": false, "error": error}))
}

#[cfg(unix)]
fn validate_base_url(value: Option<&Value>) -> Result<(), String> {
    let raw = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL);
    let url = Url::parse(raw).map_err(|_| "base_url_must_be_loopback_http".to_owned())?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if url.scheme() != "http" || !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return Err("base_url_must_be_loopback_http".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
fn validate_proxy_path(value: Option<&Value>) -> Result<String, String> {
    let raw = value.and_then(Value::as_str).unwrap_or("/");
    let base = Url::parse(DEFAULT_BASE_URL).expect("valid native-host base URL");
    let url = base
        .join(raw)
        .map_err(|_| "proxy_path_not_allowed".to_owned())?;
    if !ALLOWED_PROXY_PATHS.contains(&url.path()) {
        return Err("proxy_path_not_allowed".to_owned());
    }
    let mut path = url.path().to_owned();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    Ok(path)
}

#[cfg(unix)]
fn timeout_duration(value: Option<&Value>) -> Duration {
    let millis = value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
        })
        .filter(|value| value.is_finite())
        .unwrap_or(DEFAULT_TIMEOUT_MS as f64)
        .clamp(MIN_TIMEOUT_MS as f64, MAX_TIMEOUT_MS as f64) as u64;
    Duration::from_millis(millis)
}

#[cfg(unix)]
fn body_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
    }
}

#[cfg(unix)]
fn inject_browser_family(
    path: &str,
    method: &Method,
    body: String,
    browser_family: &str,
) -> Result<String, String> {
    if path != "/extension/browser/registry" || method != Method::POST || body.is_empty() {
        return Ok(body);
    }
    let mut payload: Value = serde_json::from_str(&body)
        .map_err(|_| "native_browser_registry_body_invalid".to_owned())?;
    if payload.get("operation").and_then(Value::as_str) != Some("endpoint.register") {
        return Ok(body);
    }
    let object = payload
        .as_object_mut()
        .ok_or_else(|| "native_browser_registry_body_invalid".to_owned())?;
    object.insert("browser_family".to_owned(), json!(browser_family));
    serde_json::to_string(&payload).map_err(|_| "native_browser_registry_body_invalid".to_owned())
}

#[cfg(unix)]
fn proxy_method(value: Option<&Value>) -> Result<Method, String> {
    let method = value
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    match method.as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "DELETE" => Ok(Method::DELETE),
        _ => Err("proxy_method_not_allowed".to_owned()),
    }
}

#[cfg(unix)]
fn sanitized_headers(value: Option<&Value>) -> Result<Vec<(HeaderName, HeaderValue)>, String> {
    let Some(headers) = value.and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if !FORWARDED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        let name = HeaderName::from_bytes(lower.as_bytes())
            .map_err(|_| "invalid_proxy_header".to_owned())?;
        let value = HeaderValue::from_str(&text).map_err(|_| "invalid_proxy_header".to_owned())?;
        output.push((name, value));
    }
    Ok(output)
}

#[cfg(unix)]
async fn send_unix_request(
    config: &HostConfig,
    path: &str,
    method: Method,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: String,
    connect_timeout: Duration,
) -> Result<Response<Incoming>, String> {
    let stream = timeout(connect_timeout, UnixStream::connect(&config.socket_path))
        .await
        .map_err(|_| "native_request_timeout".to_owned())?
        .map_err(|_| "extension_ipc_unavailable".to_owned())?;
    let io = TokioIo::new(stream);
    let (mut sender, connection) = timeout(connect_timeout, http1::handshake(io))
        .await
        .map_err(|_| "native_request_timeout".to_owned())?
        .map_err(|_| "extension_ipc_unavailable".to_owned())?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, "localhost");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| "native_request_invalid".to_owned())?;
    timeout(connect_timeout, sender.send_request(request))
        .await
        .map_err(|_| "native_request_timeout".to_owned())?
        .map_err(|_| "extension_ipc_unavailable".to_owned())
}

#[cfg(unix)]
async fn collect_body(mut body: Incoming, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| "native_response_failed".to_owned())?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if output.len().saturating_add(data.len()) > max_bytes {
            return Err("native_response_too_large".to_owned());
        }
        output.extend_from_slice(&data);
    }
    Ok(output)
}

#[cfg(unix)]
fn projected_response_headers(response: &Response<Incoming>) -> Value {
    let mut headers = Map::new();
    headers.insert(
        "content-type".to_owned(),
        json!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
        ),
    );
    headers.insert(
        "cache-control".to_owned(),
        json!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
        ),
    );
    Value::Object(headers)
}

#[cfg(unix)]
async fn proxy_request(config: &HostConfig, message: &Value) -> Result<Value, String> {
    require_active_owner(config)?;
    validate_base_url(message.get("base_url"))?;
    let path = validate_proxy_path(message.get("path"))?;
    let method = proxy_method(message.get("method"))?;
    let body = inject_browser_family(
        &path,
        &method,
        body_text(message.get("body")),
        &config.browser_family,
    )?;
    if body.len() > MAX_REQUEST_BODY {
        return Err("native_request_too_large".to_owned());
    }
    let request_timeout = timeout_duration(message.get("timeout_ms"));
    let headers = sanitized_headers(message.get("headers"))?;
    let response = send_unix_request(config, &path, method, headers, body, request_timeout).await?;
    let status = response.status().as_u16();
    let headers = projected_response_headers(&response);
    let body = tokio::time::timeout(
        request_timeout,
        collect_body(response.into_body(), MAX_RESPONSE_BODY),
    )
    .await
    .map_err(|_| "native_request_timeout".to_owned())??;
    Ok(json!({
        "ok": true,
        "transport": "ipc",
        "status": status,
        "headers": headers,
        "body": String::from_utf8_lossy(&body),
    }))
}

#[cfg(unix)]
async fn proxy_request_batch(config: &HostConfig, message: &Value) -> Result<Value, String> {
    let requests = message
        .get("requests")
        .and_then(Value::as_array)
        .ok_or_else(|| "native_request_batch_invalid".to_owned())?;
    if requests.is_empty() {
        return Err("native_request_batch_empty".to_owned());
    }
    if requests.len() > MAX_NATIVE_REQUEST_BATCH {
        return Err("native_request_batch_too_large".to_owned());
    }
    if requests.iter().any(|request| !request.is_object()) {
        return Err("native_request_batch_invalid".to_owned());
    }
    let responses = stream::iter(requests.iter().cloned().map(|request| async move {
        match proxy_request(config, &request).await {
            Ok(value) => value,
            Err(error) => json!({"ok": false, "error": error}),
        }
    }))
    .buffered(MAX_NATIVE_REQUEST_BATCH_PARALLEL)
    .collect::<Vec<_>>()
    .await;
    Ok(json!({"ok": true, "responses": responses}))
}

#[cfg(unix)]
async fn proxy_stream<W: Write>(
    config: &HostConfig,
    message: &Value,
    writer: &mut W,
) -> Result<(), String> {
    require_active_owner(config)?;
    validate_base_url(message.get("base_url"))?;
    let path = match message.get("path") {
        Some(path) => validate_proxy_path(Some(path))?,
        None => "/push/events".to_owned(),
    };
    if !path.starts_with("/push/events") {
        return Err("stream_path_not_allowed".to_owned());
    }
    let timeout = timeout_duration(message.get("timeout_ms"));
    let response = send_unix_request(
        config,
        &path,
        Method::GET,
        vec![(ACCEPT, HeaderValue::from_static("text/event-stream"))],
        String::new(),
        timeout,
    )
    .await
    .map_err(|error| {
        if error == "native_request_timeout" {
            "native_stream_connect_timeout".to_owned()
        } else {
            error
        }
    })?;
    let status = response.status().as_u16();
    write_native_message(
        writer,
        &json!({"type": "stream_open", "status": status, "transport": "ipc"}),
    )?;
    if status >= 400 {
        let _ = collect_body(response.into_body(), MAX_RESPONSE_BODY).await;
        return Ok(());
    }

    let mut body = response.into_body();
    let mut owner_fence = interval(Duration::from_secs(1));
    owner_fence.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = owner_fence.tick() => {
                require_active_owner(config)?;
            }
            frame = body.frame() => {
                let Some(frame) = frame else {
                    break;
                };
                require_active_owner(config)?;
                let frame = frame.map_err(|_| "native_stream_failed".to_owned())?;
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                for chunk in data.chunks(STREAM_CHUNK_BYTES) {
                    write_native_message(
                        writer,
                        &json!({"type": "stream_chunk", "chunk_b64": BASE64.encode(chunk)}),
                    )?;
                }
            }
        }
    }
    write_native_message(writer, &json!({"type": "stream_end"}))
}

/// Handle a dedicated ChatGPT Web artifact capture message.
///
/// The extension sends only image bytes and non-secret identifiers; tokens,
/// cookies, and download URLs must never cross the native-message boundary. The
/// host rejects unknown fields (so secrets cannot be smuggled in), base64-decodes
/// strictly, enforces the 8 MiB raw limit and image MIME/magic, recomputes
/// SHA-256 in Rust, and stores the artifact in the secure short-lived cache.
#[cfg(unix)]
fn handle_artifact_capture(config: &HostConfig, message: &Value) -> Result<Value, String> {
    require_active_owner(config)?;

    let Some(object) = message.as_object() else {
        return Err("artifact_capture_invalid".to_owned());
    };
    const ALLOWED_FIELDS: &[&str] = &[
        "type",
        "conversation_id",
        "file_id",
        "mime",
        "bytes_b64",
        "sha256",
    ];
    // Reject unknown fields so a token/cookie/download URL accidentally sent by
    // the browser cannot cross the boundary even on an otherwise-valid message.
    if object
        .keys()
        .any(|key| !ALLOWED_FIELDS.contains(&key.as_str()))
    {
        return Err("artifact_capture_unknown_field".to_owned());
    }

    let conversation_id = string_field(message, "conversation_id")?;
    let file_id = string_field(message, "file_id")?;
    let mime = string_field(message, "mime")?;
    let encoded = string_field(message, "bytes_b64")?;
    let browser_sha256 = match message.get("sha256") {
        Some(Value::String(value)) if !value.is_empty() => Some(value.as_str()),
        None => None,
        Some(_) => return Err("artifact_capture_invalid_sha256".to_owned()),
    };

    if encoded.len() > 16 * 1024 * 1024 {
        return Err("artifact_capture_too_large".to_owned());
    }
    let raw = BASE64
        .decode(encoded)
        .map_err(|_| "artifact_capture_invalid_base64".to_owned())?;

    let paths = crate::paths::RuntimePaths::discover()?;
    let captured = crate::web_artifact_cache::capture(
        &paths.config_dir,
        conversation_id,
        file_id,
        mime,
        &raw,
        browser_sha256,
    )?;

    Ok(json!({
        "ok": true,
        "type": "artifact_capture",
        "artifact": captured.metadata_json(),
    }))
}

#[cfg(unix)]
fn string_field<'a>(message: &'a Value, key: &str) -> Result<&'a str, String> {
    message
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("artifact_capture_missing_{key}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(unix)]
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn socket_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        env::temp_dir().join(format!(
            "hmh-{label}-{:x}-{:x}-{nonce:x}.sock",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn frame(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut output = (body.len() as u32).to_le_bytes().to_vec();
        output.extend_from_slice(&body);
        output
    }

    fn parse_frames(mut bytes: &[u8]) -> Vec<Value> {
        let mut output = Vec::new();
        while bytes.len() >= 4 {
            let size = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
            bytes = &bytes[4..];
            if bytes.len() < size {
                break;
            }
            output.push(serde_json::from_slice(&bytes[..size]).unwrap());
            bytes = &bytes[size..];
        }
        output
    }

    #[test]
    fn native_message_framing_round_trips_and_is_bounded() {
        let value = json!({"type":"request","path":"/push/state"});
        let encoded = frame(&value);
        let encoded_len = encoded.len() - 4;
        let (parsed, size) = read_native_message(&mut Cursor::new(encoded)).unwrap();
        assert_eq!(parsed, value);
        assert_eq!(size, encoded_len);

        // Frames beyond the dedicated 16 MiB envelope are rejected outright.
        let mut beyond_frame = ((MAX_NATIVE_MESSAGE_FRAME + 1) as u32)
            .to_le_bytes()
            .to_vec();
        beyond_frame.extend_from_slice(b"{}");
        assert_eq!(
            read_native_message(&mut Cursor::new(beyond_frame)).unwrap_err(),
            "native_message_too_large"
        );

        // A generic-typed message of 1 MiB+ parses at the framing gate; the
        // per-type 1 MiB cap is enforced in `run` and is covered there.
        let big_body = vec![b' '; MAX_NATIVE_MESSAGE + 1];
        let generic_big = frame(&json!({"body": big_body, "type": "request"}));
        let (_, generic_size) = read_native_message(&mut Cursor::new(generic_big)).unwrap();
        assert!(generic_size > MAX_NATIVE_MESSAGE);

        // An artifact-typed frame under the 16 MiB bound always parses; the
        // per-type gate (and payload limits) are decided in `handle_artifact_capture`.
        let payload_len = MAX_NATIVE_MESSAGE + 8;
        let big_payload = vec![b'a'; payload_len];
        let big = json!({"type":"artifact_capture","bytes_b64": big_payload});
        let big_encoded = frame(&big);
        let (parsed_big, big_size) = read_native_message(&mut Cursor::new(big_encoded)).unwrap();
        assert_eq!(parsed_big["type"], "artifact_capture");
        assert!(big_size > MAX_NATIVE_MESSAGE);
        assert!(big_size <= MAX_NATIVE_MESSAGE_FRAME);
    }

    #[test]
    fn per_type_size_gate_keeps_generic_messages_at_one_megabyte() {
        // The per-type gate in `run` admits only `artifact_capture` beyond 1 MiB.
        assert_eq!(
            enforce_message_size_gate("artifact_capture", MAX_NATIVE_MESSAGE + 8),
            None
        );
        assert_eq!(
            enforce_message_size_gate("request", MAX_NATIVE_MESSAGE),
            None
        );
        const { assert!(MAX_NATIVE_MESSAGE_FRAME > MAX_NATIVE_MESSAGE) };

        // A generic message over 1 MiB is refused by the gate.
        let over_size = MAX_NATIVE_MESSAGE + 4;
        assert_eq!(
            enforce_message_size_gate("request", over_size).unwrap(),
            "native_message_too_large"
        );
        assert_eq!(
            enforce_message_size_gate("stream", over_size).unwrap(),
            "native_message_too_large"
        );
    }

    #[test]
    fn proxy_validation_is_loopback_and_allowlist_only() {
        assert!(validate_base_url(Some(&json!("http://127.0.0.1:8772"))).is_ok());
        assert!(validate_base_url(Some(&json!("http://localhost:8772/path"))).is_ok());
        assert!(validate_base_url(Some(&json!("https://127.0.0.1:8772"))).is_err());
        assert!(validate_base_url(Some(&json!("http://example.com"))).is_err());
        assert_eq!(
            validate_proxy_path(Some(&json!("/push/events?workspace=w77"))).unwrap(),
            "/push/events?workspace=w77"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/extension/fleet"))).unwrap(),
            "/extension/fleet"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/extension/browser/registry"))).unwrap(),
            "/extension/browser/registry"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/extension/browser/actuation"))).unwrap(),
            "/extension/browser/actuation"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/extension/continuity/turn"))).unwrap(),
            "/extension/continuity/turn"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/extension/continuity/resolve"))).unwrap(),
            "/extension/continuity/resolve"
        );
        assert_eq!(
            validate_proxy_path(Some(&json!("/oauth/token"))).unwrap_err(),
            "proxy_path_not_allowed"
        );
    }

    #[test]
    fn chromium_path_identity_is_stable_and_origin_is_strict() {
        let id = chromium_id_for_path(Path::new("/tmp/herdr-extension")).unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)));
        assert_eq!(
            chromium_id_for_path(Path::new("/tmp/herdr-extension")).unwrap(),
            id
        );
        assert!(validate_extension_origin(&format!("chrome-extension://{id}/")).is_ok());
        assert!(validate_extension_origin("https://example.com/").is_err());
    }

    #[test]
    fn active_owner_fence_requires_the_exact_current_origin() {
        let expected = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/";
        let other = "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/";
        assert!(owner_is_active(expected, Some(expected)));
        assert!(!owner_is_active(expected, Some(other)));
        assert!(!owner_is_active(expected, None));
    }

    #[test]
    fn browser_family_is_derived_from_the_native_host_parent_process() {
        assert_eq!(
            classify_browser_family("/Applications/ego lite.app/Contents/MacOS/ego lite"),
            "ego"
        );
        assert_eq!(
            classify_browser_family(
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"
            ),
            "edge"
        );
        assert_eq!(
            classify_browser_family("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            "chrome"
        );
        assert_eq!(
            classify_browser_family("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
            "brave"
        );
        assert_eq!(classify_browser_family("/usr/bin/chromium"), "chromium");
        assert_eq!(classify_browser_family("/opt/custom-browser"), "chromium");
    }

    #[test]
    fn browser_registry_registration_gets_host_derived_family() {
        let body = inject_browser_family(
            "/extension/browser/registry",
            &Method::POST,
            json!({
                "operation": "endpoint.register",
                "profile_seed": "seed",
                "browser_family": "chrome",
                "extension_version": "0.1.91"
            })
            .to_string(),
            "ego",
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["browser_family"], "ego");

        let untouched = inject_browser_family(
            "/extension/browser/registry",
            &Method::POST,
            json!({"operation": "endpoint.consent", "browser_family": "chrome"}).to_string(),
            "ego",
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&untouched).unwrap()["browser_family"],
            "chrome"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extension_dir_helpers_require_manifest_json() {
        let root = env::temp_dir().join(format!(
            "herdr-ext-dir-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let good = root.join("good");
        let empty = root.join("empty");
        fs::create_dir_all(&good).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::write(good.join("manifest.json"), b"{\"version\":\"1\"}").unwrap();
        assert!(is_extension_dir(&good));
        assert!(!is_extension_dir(&empty));
        assert_eq!(require_extension_dir(&good, "ok").unwrap(), good);
        let err = require_extension_dir(
            &empty,
            "HERDR_EXTENSION_PATH points to a missing or incomplete extension directory",
        )
        .unwrap_err();
        assert!(err.contains("HERDR_EXTENSION_PATH"));
        assert!(err.contains("manifest.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_proxy_uses_tokenless_unix_ipc_and_strips_authorization() {
        let path = socket_path("request");
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).unwrap();
            let text = String::from_utf8_lossy(&request[..read]);
            assert!(text.starts_with("GET /push/state HTTP/1.1"));
            assert!(!text.to_ascii_lowercase().contains("authorization:"));
            let body = r#"{"ok":true,"transport":"socket"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let config = HostConfig {
            expected_origin: "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/".to_owned(),
            socket_path: path.clone(),
            browser_family: "chrome".to_owned(),
            enforce_owner_fence: false,
        };
        let result = proxy_request(
            &config,
            &json!({
                "type":"request",
                "base_url":"http://127.0.0.1:8772",
                "path":"/push/state",
                "method":"GET",
                "headers":{"Authorization":"Bearer must-be-stripped"}
            }),
        )
        .await
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["transport"], "ipc");
        assert_eq!(result["status"], 200);
        assert_eq!(
            serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["transport"],
            "socket"
        );
        server.join().unwrap();
        fs::remove_file(path).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn request_batch_proxy_preserves_order_and_rejects_oversize() {
        let path = socket_path("request-batch");
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 8192];
                let read = stream.read(&mut request).unwrap();
                let text = String::from_utf8_lossy(&request[..read]);
                let observed_path = if text.starts_with("GET /push/state HTTP/1.1") {
                    "/push/state"
                } else if text.starts_with("GET /extension/fleet HTTP/1.1") {
                    "/extension/fleet"
                } else {
                    panic!("unexpected batched native request: {text}");
                };
                let body = format!(r#"{{"path":"{observed_path}"}}"#);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let config = HostConfig {
            expected_origin: "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/".to_owned(),
            socket_path: path.clone(),
            browser_family: "chrome".to_owned(),
            enforce_owner_fence: false,
        };
        let result = proxy_request_batch(
            &config,
            &json!({
                "type":"request_batch",
                "requests":[
                    {
                        "base_url":"http://127.0.0.1:8772",
                        "path":"/push/state",
                        "method":"GET"
                    },
                    {
                        "base_url":"http://127.0.0.1:8772",
                        "path":"/extension/fleet",
                        "method":"GET"
                    }
                ]
            }),
        )
        .await
        .unwrap();
        let responses = result["responses"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(responses[0]["body"].as_str().unwrap()).unwrap()["path"],
            "/push/state"
        );
        assert_eq!(
            serde_json::from_str::<Value>(responses[1]["body"].as_str().unwrap()).unwrap()["path"],
            "/extension/fleet"
        );
        server.join().unwrap();
        fs::remove_file(&path).ok();

        let oversized = vec![
            json!({
                "base_url":"http://127.0.0.1:8772",
                "path":"/push/state",
                "method":"GET"
            });
            MAX_NATIVE_REQUEST_BATCH + 1
        ];
        assert_eq!(
            proxy_request_batch(&config, &json!({"requests": oversized}))
                .await
                .unwrap_err(),
            "native_request_batch_too_large"
        );
    }

    #[cfg(unix)]
    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "hmh-host-{label}-{:x}-{:x}-{nonce:x}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Serve exactly one `GET /push/state` request so a proxied native message
    /// can complete for real instead of being faked at the proxy boundary.
    #[cfg(unix)]
    fn spawn_state_server(path: &Path) -> thread::JoinHandle<()> {
        fs::remove_file(path).ok();
        let listener = UnixListener::bind(path).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /push/state"));
            let body = r#"{"ok":true,"transport":"socket"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        })
    }

    #[cfg(unix)]
    fn test_config(socket_path: PathBuf, enforce_owner_fence: bool) -> HostConfig {
        HostConfig {
            expected_origin: "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/".to_owned(),
            socket_path,
            browser_family: "chrome".to_owned(),
            enforce_owner_fence,
        }
    }

    #[cfg(unix)]
    fn read_log(log: &NativeHostLog) -> String {
        fs::read_to_string(log.path()).unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn native_host_log_records_connection_metadata_without_message_content() {
        const SECRET: &str = "super-secret-token-xyz";
        let root = temp_dir("connect");
        let socket = socket_path("connect-log");
        let server = spawn_state_server(&socket);
        let log = NativeHostLog::in_config_dir(&root);
        let config = test_config(socket.clone(), false);
        let message = json!({
            "type": "request",
            "base_url": "http://127.0.0.1:8772",
            "path": format!("/push/state?workspace={SECRET}"),
            "method": "GET",
            "headers": {"Authorization": format!("Bearer {SECRET}")},
            "body": SECRET,
        });
        let mut reader = Cursor::new(frame(&message));
        let mut writer = Vec::new();

        let outcome = serve(
            &config.expected_origin,
            &mut reader,
            &mut writer,
            Some(&log),
            || Ok(config.clone()),
        );
        assert!(outcome.is_ok(), "native host must answer the browser");
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["ok"], true);
        assert_eq!(response["status"], 200);
        server.join().unwrap();

        let written = read_log(&log);
        assert!(written.contains("event=connect"), "{written}");
        assert!(
            written.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))),
            "{written}"
        );
        assert!(
            written
                .contains("extension_origin=chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"),
            "{written}"
        );
        assert!(written.contains("message_type=request"), "{written}");
        assert!(written.contains("encoded_bytes="), "{written}");
        assert!(written.contains("event=disconnect"), "{written}");
        assert!(written.contains("reason=completed"), "{written}");
        assert!(written.contains("duration_ms="), "{written}");
        for forbidden in [SECRET, "Bearer", "workspace=", "Authorization"] {
            assert!(
                !written.contains(forbidden),
                "log leaked {forbidden}: {written}"
            );
        }
        fs::remove_file(socket).ok();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_host_log_records_ipc_failure_without_failing_the_host() {
        let root = temp_dir("ipc-failure");
        let log = NativeHostLog::in_config_dir(&root);
        // No listener exists at this path, so the proxy reports a dead IPC
        // socket the same way it does in production.
        let missing = root.join("missing.sock");
        let config = test_config(missing, false);
        let mut reader = Cursor::new(frame(&json!({
            "type": "request",
            "path": "/push/state",
            "method": "GET",
        })));
        let mut writer = Vec::new();

        let outcome = serve(
            &config.expected_origin,
            &mut reader,
            &mut writer,
            Some(&log),
            || Ok(config.clone()),
        );
        assert!(outcome.is_ok(), "a dead socket must not fail the host");
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"], "extension_ipc_unavailable");

        let written = read_log(&log);
        assert!(written.contains("event=connect"), "{written}");
        assert!(written.contains("event=io_error"), "{written}");
        assert!(
            written.contains("code=extension_ipc_unavailable"),
            "{written}"
        );
        assert!(
            written.contains("reason=extension_ipc_unavailable"),
            "{written}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_host_serves_the_browser_when_the_log_cannot_be_written() {
        const SECRET: &str = "super-secret-token-xyz";
        let root = temp_dir("log-failure");
        // A regular file where the config root must be makes every log write
        // fail; the browser response must still be served.
        let unusable = root.join("not-a-directory");
        fs::write(&unusable, b"not a directory").unwrap();
        let socket = socket_path("log-failure");
        let server = spawn_state_server(&socket);
        let log = NativeHostLog::in_config_dir(&unusable);
        let config = test_config(socket.clone(), false);
        let mut reader = Cursor::new(frame(&json!({
            "type": "request",
            "path": "/push/state",
            "method": "GET",
            "body": SECRET,
        })));
        let mut writer = Vec::new();

        let outcome = serve(
            &config.expected_origin,
            &mut reader,
            &mut writer,
            Some(&log),
            || Ok(config.clone()),
        );
        assert!(outcome.is_ok(), "a log failure must not fail the host");
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["ok"], true);
        assert_eq!(response["status"], 200);
        assert!(!read_log(&log).contains(SECRET));
        server.join().unwrap();
        fs::remove_file(socket).ok();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_host_log_rejects_a_foreign_origin_before_the_registry_fence() {
        const SECRET: &str = "super-secret-token-xyz";
        let root = temp_dir("origin");
        let log = NativeHostLog::in_config_dir(&root);
        // `enforce_owner_fence` stays on: this path must refuse before it ever
        // consults the installed extension registry.
        let config = test_config(root.join("unused.sock"), true);
        let foreign = "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/";
        let mut reader = Cursor::new(frame(&json!({
            "type": "request",
            "path": "/push/state",
            "method": "GET",
        })));
        let mut writer = Vec::new();

        let outcome = serve(foreign, &mut reader, &mut writer, Some(&log), || {
            Ok(config.clone())
        });
        assert!(outcome.is_ok());
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["error"], "extension_origin_not_allowed");

        let written = read_log(&log);
        assert!(
            written.contains(&format!("extension_origin={foreign}")),
            "{written}"
        );
        assert!(
            written.contains("reason=extension_origin_not_allowed"),
            "{written}"
        );
        assert!(!written.contains(SECRET));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_host_log_records_unknown_types_and_origins_as_placeholders() {
        const SECRET: &str = "super-secret-token-xyz";
        let root = temp_dir("allowlist");
        let log = NativeHostLog::in_config_dir(&root);
        let config = test_config(root.join("unused.sock"), false);

        // A non-extension caller origin is exactly the shape an
        // attacker-controlled value would take, so it is recorded as `-`.
        let caller = format!("https://example.com/?token={SECRET}");
        let mut reader = Cursor::new(frame(&json!({
            "type": "request",
            "path": "/push/state",
            "method": "GET",
        })));
        let mut writer = Vec::new();
        let outcome = serve(&caller, &mut reader, &mut writer, Some(&log), || {
            Ok(config.clone())
        });
        assert!(outcome.is_ok());
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["error"], "extension_origin_not_allowed");

        // An unknown message type is recorded as `other`.
        let mut reader = Cursor::new(frame(&json!({"type": SECRET})));
        let mut writer = Vec::new();
        let outcome = serve(
            &config.expected_origin,
            &mut reader,
            &mut writer,
            Some(&log),
            || Ok(config.clone()),
        );
        assert!(outcome.is_ok());
        let response = parse_frames(&writer).remove(0);
        assert_eq!(response["error"], "unsupported_message");

        let written = read_log(&log);
        assert!(written.contains("extension_origin=- "), "{written}");
        assert!(
            written.contains("reason=extension_origin_not_allowed"),
            "{written}"
        );
        assert!(written.contains("message_type=other"), "{written}");
        assert!(written.contains("reason=unsupported_message"), "{written}");
        assert!(!written.contains(SECRET), "{written}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stream_proxy_frames_sse_bytes_without_buffering_the_stream() {
        let path = socket_path("stream");
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let text = String::from_utf8_lossy(&request[..read]);
            assert!(text.starts_with("GET /push/events HTTP/1.1"));
            assert!(
                text.to_ascii_lowercase()
                    .contains("accept: text/event-stream")
            );
            let body = "event: hello\ndata: {\"ok\":true}\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let config = HostConfig {
            expected_origin: "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/".to_owned(),
            socket_path: path.clone(),
            browser_family: "chrome".to_owned(),
            enforce_owner_fence: false,
        };
        let mut output = Vec::new();
        proxy_stream(
            &config,
            &json!({
                "type":"stream",
                "base_url":"http://127.0.0.1:8772",
                "path":"/push/events"
            }),
            &mut output,
        )
        .await
        .unwrap();
        let frames = parse_frames(&output);
        assert_eq!(frames[0]["type"], "stream_open");
        assert_eq!(frames[0]["status"], 200);
        assert_eq!(frames[0]["transport"], "ipc");
        let bytes = frames
            .iter()
            .filter(|frame| frame["type"] == "stream_chunk")
            .filter_map(|frame| frame["chunk_b64"].as_str())
            .flat_map(|encoded| BASE64.decode(encoded).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "event: hello\ndata: {\"ok\":true}\n\n"
        );
        assert_eq!(frames.last().unwrap()["type"], "stream_end");
        server.join().unwrap();
        fs::remove_file(path).ok();
    }
}
