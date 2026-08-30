//! Chrome Native Messaging data plane for the Rust runtime.
//!
//! The installer remains on the existing compatibility path for now. This
//! module implements the current extension's request/stream protocol over the
//! mode-0600 Unix socket so the browser data plane no longer needs Node once
//! the Rust runtime is activated in production.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::ExitCode;

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
const ALLOWED_PROXY_PATHS: &[&str] = &[
    "/mcp",
    "/push/state",
    "/push/events",
    "/push/mcp-activity",
    "/extension/control/action",
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
            #[cfg(test)]
            enforce_owner_fence: true,
        })
    }
}

#[cfg(target_os = "macos")]
fn owner_is_active(expected_origin: &str, registered_origin: Option<&str>) -> bool {
    registered_origin == Some(expected_origin)
}

#[cfg(unix)]
fn require_active_owner(config: &HostConfig) -> Result<(), String> {
    #[cfg(test)]
    if !config.enforce_owner_fence {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let registered = crate::native_host_install::current_registered_extension_origin()?;
        if !owner_is_active(&config.expected_origin, registered.as_deref()) {
            return Err("extension_origin_not_active".to_owned());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
    }
    Ok(())
}

pub fn run(caller_origin: &str) -> Result<ExitCode, String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let message = match read_native_message(&mut reader) {
        Ok(message) => message,
        Err(error) => {
            write_error(&mut writer, &error)?;
            return Ok(ExitCode::SUCCESS);
        }
    };

    #[cfg(not(unix))]
    {
        let _ = caller_origin;
        let _ = message;
        write_error(&mut writer, "unsupported_platform")?;
        return Ok(ExitCode::SUCCESS);
    }

    #[cfg(unix)]
    {
        let config = match HostConfig::from_env() {
            Ok(config) => config,
            Err(error) => {
                write_error(&mut writer, &error)?;
                return Ok(ExitCode::SUCCESS);
            }
        };
        let effective_caller_origin = if caller_origin.is_empty() {
            config.expected_origin.as_str()
        } else {
            caller_origin
        };
        if effective_caller_origin != config.expected_origin {
            write_error(&mut writer, "extension_origin_not_allowed")?;
            return Ok(ExitCode::SUCCESS);
        }

        let message_type = message.get("type").and_then(Value::as_str).unwrap_or("");
        if message_type == "identity" {
            let active = require_active_owner(&config).is_ok();
            write_native_message(
                &mut writer,
                &json!({
                    "ok": true,
                    "active": active,
                    "extension_origin": config.expected_origin,
                    "transport": "native",
                }),
            )?;
            return Ok(ExitCode::SUCCESS);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot build native-host runtime: {error}"))?;
        match message_type {
            "request" => match runtime.block_on(proxy_request(&config, &message)) {
                Ok(value) => write_native_message(&mut writer, &value)?,
                Err(error) => write_error(&mut writer, &error)?,
            },
            "stream" => {
                if let Err(error) = runtime.block_on(proxy_stream(&config, &message, &mut writer)) {
                    write_error(&mut writer, &error)?;
                }
            }
            "session" => write_error(&mut writer, "legacy_session_requires_compat_host")?,
            _ => write_error(&mut writer, "unsupported_message")?,
        }
        Ok(ExitCode::SUCCESS)
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

#[cfg(target_os = "macos")]
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
fn require_extension_dir(path: &Path, context: &str) -> Result<PathBuf, String> {
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
fn validate_extension_origin(origin: &str) -> Result<&str, String> {
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
fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
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

fn read_native_message<R: Read>(reader: &mut R) -> Result<Value, String> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| "invalid_native_message".to_owned())?;
    let size = u32::from_le_bytes(header) as usize;
    if size > MAX_NATIVE_MESSAGE {
        return Err("native_message_too_large".to_owned());
    }
    let mut body = vec![0_u8; size];
    reader
        .read_exact(&mut body)
        .map_err(|_| "invalid_native_message".to_owned())?;
    serde_json::from_slice(&body).map_err(|_| "invalid_native_message".to_owned())
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
    let body = body_text(message.get("body"));
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
        assert_eq!(
            read_native_message(&mut Cursor::new(encoded)).unwrap(),
            value
        );

        let mut oversized = ((MAX_NATIVE_MESSAGE + 1) as u32).to_le_bytes().to_vec();
        oversized.extend_from_slice(b"{}");
        assert_eq!(
            read_native_message(&mut Cursor::new(oversized)).unwrap_err(),
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

    #[cfg(target_os = "macos")]
    #[test]
    fn active_owner_fence_requires_the_exact_current_origin() {
        let expected = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/";
        let other = "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/";
        assert!(owner_is_active(expected, Some(expected)));
        assert!(!owner_is_active(expected, Some(other)));
        assert!(!owner_is_active(expected, None));
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
