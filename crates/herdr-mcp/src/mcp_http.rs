use crate::exec_sessions::ExecRegistry;
use crate::herdr::HerdrClient;
use crate::mcp::{self, RuntimeContext};
use crate::paths::RuntimePaths;
use crate::prompt::PromptRegistry;
use crate::runtime_meta;
use crate::skill::SkillService;
use crate::state_cache::EventCache;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
struct SessionRegistry {
    inner: Arc<Mutex<HashMap<String, Instant>>>,
}

impl SessionRegistry {
    fn issue(&self, boot_id: &str) -> String {
        let id = new_session_id(boot_id);
        if let Ok(mut sessions) = self.inner.lock() {
            prune_sessions(&mut sessions);
            if sessions.len() >= MAX_SESSIONS
                && let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, seen)| **seen)
                    .map(|(id, _)| id.clone())
            {
                sessions.remove(&oldest);
            }
            sessions.insert(id.clone(), Instant::now());
        }
        id
    }

    fn contains_touch(&self, id: &str) -> bool {
        let Ok(mut sessions) = self.inner.lock() else {
            return false;
        };
        prune_sessions(&mut sessions);
        let Some(seen) = sessions.get_mut(id) else {
            return false;
        };
        *seen = Instant::now();
        true
    }

    fn remove(&self, id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(id))
            .is_some()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or(0)
    }
}

fn prune_sessions(sessions: &mut HashMap<String, Instant>) {
    sessions.retain(|_, seen| seen.elapsed() <= SESSION_TTL);
}

fn new_session_id(boot_id: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let mut hash = Sha256::new();
    hash.update(boot_id.as_bytes());
    hash.update(std::process::id().to_le_bytes());
    hash.update(now.to_le_bytes());
    hash.update(sequence.to_le_bytes());
    let digest = hash.finalize();
    let token = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("ms_{token}")
}

#[derive(Clone)]
struct AppState {
    client: HerdrClient,
    cache: Arc<EventCache>,
    exec: ExecRegistry,
    prompt: PromptRegistry,
    skill: SkillService,
    sessions: SessionRegistry,
    bearer_token: Arc<[u8]>,
}

pub fn serve_candidate(port: u16) -> Result<ExitCode, String> {
    let token = env::var("HERDR_MCP_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "candidate runtime requires HERDR_MCP_TOKEN; refusing unauthenticated HTTP server"
                .to_owned()
        })?;
    let paths = RuntimePaths::discover()?;
    let socket = paths
        .herdr_socket
        .ok_or_else(|| "candidate runtime requires a Herdr local transport".to_owned())?;
    let client = HerdrClient::new(socket);
    let cache = Arc::new(EventCache::start(client.clone()));
    let exec_state_dir = env::var_os("HERDR_MCP_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths.dev_state_dir.join("candidate"));
    let exec = ExecRegistry::new(exec_state_dir)?;
    let prompt = PromptRegistry::new();
    let skill = SkillService::new();
    if !cache.wait_ready(Duration::from_secs(3)) {
        return Err(format!(
            "candidate event cache did not bootstrap: {}",
            cache
                .last_error()
                .unwrap_or_else(|| "unknown error".to_owned())
        ));
    }
    if !cache.wait_stream_live(Duration::from_secs(2)) {
        return Err(format!(
            "candidate event cache did not establish events.subscribe: {}",
            cache
                .last_error()
                .unwrap_or_else(|| "unknown error".to_owned())
        ));
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot build candidate Tokio runtime: {error}"))?;
    runtime.block_on(async move {
        let state = AppState {
            client,
            cache,
            exec,
            prompt,
            skill,
            sessions: SessionRegistry::default(),
            bearer_token: Arc::<[u8]>::from(token.into_bytes()),
        };
        let app = candidate_router(state);
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| format!("cannot bind candidate runtime to {address}: {error}"))?;
        println!(
            "herdr-mcp candidate {} listening on http://{address}/mcp",
            env!("CARGO_PKG_VERSION")
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|error| format!("candidate HTTP server failed: {error}"))?;
        Ok(ExitCode::SUCCESS)
    })
}

fn candidate_router(state: AppState) -> Router {
    Router::new()
        .route("/", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/mcp/", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/health", get(health))
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health(State(state): State<AppState>) -> Response {
    let mut payload = runtime_meta::health_fields(&state.cache, Some(&state.exec));
    payload.insert("ok".to_owned(), json!(true));
    let payload = Value::Object(payload);
    json_response(StatusCode::OK, &payload)
}

async fn get_mcp(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.bearer_token) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Unauthorized"},
                "id": null
            }),
        );
    }
    if is_stateless_client(&headers, None) {
        return persistent_sse_response();
    }
    if let Some(session) = session_id(&headers)
        && state.sessions.contains_touch(session)
    {
        return persistent_sse_response();
    }
    json_response(
        StatusCode::BAD_REQUEST,
        &json!({
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": "Bad Request: session required for GET stream"},
            "id": null
        }),
    )
}

async fn delete_mcp(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.bearer_token) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Unauthorized"},
                "id": null
            }),
        );
    }
    if is_stateless_client(&headers, None) {
        return StatusCode::ACCEPTED.into_response();
    }
    let Some(session) = session_id(&headers) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32000, "message": "Bad Request: session required"},
                "id": null
            }),
        );
    };
    if state.sessions.remove(session) {
        StatusCode::ACCEPTED.into_response()
    } else {
        json_response(
            StatusCode::NOT_FOUND,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Session not found"},
                "id": null
            }),
        )
    }
}

async fn post_mcp(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &state.bearer_token) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Unauthorized"},
                "id": null
            }),
        );
    }
    if body.len() > MAX_REQUEST_BYTES {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": "Request body too large"},
                "id": null
            }),
        );
    }
    let request: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if !value.is_array() => value,
        Ok(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": "Batch requests are not supported by the Rust candidate"},
                    "id": null
                }),
            );
        }
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {error}")},
                    "id": null
                }),
            );
        }
    };

    let stateless = is_stateless_client(&headers, Some(&request));
    let request_session = session_id(&headers).map(str::to_owned);
    if !stateless
        && let Some(session) = request_session.as_deref()
        && !state.sessions.contains_touch(session)
    {
        return json_response(
            StatusCode::NOT_FOUND,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32001, "message": "Session not found"},
                "id": request.get("id").cloned().unwrap_or(Value::Null)
            }),
        );
    }

    let context = RuntimeContext {
        client: &state.client,
        cache: &state.cache,
        exec: &state.exec,
        prompt: &state.prompt,
        skill: &state.skill,
    };
    let Some(mut response) = mcp::handle(&request, &context) else {
        return StatusCode::ACCEPTED.into_response();
    };
    if stateless && request.get("method").and_then(Value::as_str) == Some("server/discover") {
        augment_openai_discover(&mut response);
    }
    let issued_session = if !stateless
        && request_session.is_none()
        && request.get("method").and_then(Value::as_str) == Some("initialize")
        && response.get("result").is_some()
    {
        Some(state.sessions.issue(state.cache.boot_id()))
    } else {
        None
    };
    if wants_sse(&headers, &request) {
        sse_response(&response, issued_session.as_deref())
    } else {
        json_response_with_session(StatusCode::OK, &response, issued_session.as_deref())
    }
}

fn is_stateless_client(headers: &HeaderMap, request: Option<&Value>) -> bool {
    if headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("openai-mcp"))
    {
        return true;
    }
    request
        .and_then(|request| request.pointer("/params/clientInfo/name"))
        .and_then(Value::as_str)
        .is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name == "chatgpt" || name.contains("openai") || name.contains("chatgpt")
        })
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

fn augment_openai_discover(response: &mut Value) {
    let Some(versions) = response
        .pointer_mut("/result/supportedVersions")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    if !versions.iter().any(|value| value == "2026-07-28") {
        versions.push(json!("2026-07-28"));
    }
}

fn wants_sse(headers: &HeaderMap, request: &Value) -> bool {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if !matches!(method, "initialize" | "tools/list") {
        return false;
    }
    headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

fn authorized(headers: &HeaderMap, token: &[u8]) -> bool {
    let Some(value) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(provided) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(provided.as_bytes(), token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    json_response_with_session(status, value, None)
}

fn json_response_with_session(
    status: StatusCode,
    value: &Value,
    session_id: Option<&str>,
) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(session_id) = session_id {
        builder = builder.header("mcp-session-id", session_id);
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn sse_response(value: &Value, session_id: Option<&str>) -> Response {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    let body = format!("event: message\ndata: {json}\n\n");
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header("cache-control", HeaderValue::from_static("no-cache"));
    if let Some(session_id) = session_id {
        builder = builder.header("mcp-session-id", session_id);
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn persistent_sse_response() -> Response {
    let events = stream::unfold(true, |first| async move {
        if first {
            return Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(b": connected\n\n")),
                false,
            ));
        }
        tokio::time::sleep(SSE_HEARTBEAT).await;
        Some((
            Ok::<Bytes, Infallible>(Bytes::from_static(b": keepalive\n\n")),
            false,
        ))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header(
            "cache-control",
            HeaderValue::from_static("no-cache, no-transform"),
        )
        .header("connection", HeaderValue::from_static("keep-alive"))
        .header("x-accel-buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(events))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!(
            "herdr-mcp-http-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn test_state(root: &std::path::Path) -> AppState {
        AppState {
            client: HerdrClient::new(root.join("missing-herdr.sock")),
            cache: Arc::new(EventCache::from_snapshot_for_test(json!({
                "workspaces": [],
                "panes": [],
                "agents": []
            }))),
            exec: ExecRegistry::new(root.join("exec")).unwrap(),
            prompt: PromptRegistry::new(),
            skill: SkillService::new(),
            sessions: SessionRegistry::default(),
            bearer_token: Arc::<[u8]>::from(b"test-token".to_vec()),
        }
    }

    fn rpc_request(
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, "Bearer test-token")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(match body {
                Some(body) => Body::from(body.to_string()),
                None => Body::empty(),
            })
            .unwrap()
    }

    #[test]
    fn bearer_check_is_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert!(authorized(&headers, b"secret"));
        assert!(!authorized(&headers, b"other"));
        assert!(!authorized(&HeaderMap::new(), b"secret"));
    }

    #[test]
    fn handshake_uses_sse_when_client_accepts_it() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        assert!(wants_sse(&headers, &json!({"method": "initialize"})));
        assert!(wants_sse(&headers, &json!({"method": "tools/list"})));
        assert!(!wants_sse(&headers, &json!({"method": "tools/call"})));
    }

    #[test]
    fn openai_or_chatgpt_clients_are_stateless() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static("openai-mcp/1.0.0"));
        assert!(is_stateless_client(&headers, None));

        let request = json!({
            "method": "initialize",
            "params": {"clientInfo": {"name": "ChatGPT"}}
        });
        assert!(is_stateless_client(&HeaderMap::new(), Some(&request)));

        let normal = json!({
            "method": "initialize",
            "params": {"clientInfo": {"name": "claude-desktop"}}
        });
        assert!(!is_stateless_client(&HeaderMap::new(), Some(&normal)));
    }

    #[test]
    fn session_registry_is_bounded_and_removable() {
        let sessions = SessionRegistry::default();
        let first = sessions.issue("boot-test");
        assert!(sessions.contains_touch(&first));
        assert!(sessions.remove(&first));
        assert!(!sessions.contains_touch(&first));

        for _ in 0..=MAX_SESSIONS {
            sessions.issue("boot-test");
        }
        assert!(sessions.len() <= MAX_SESSIONS);
    }

    #[test]
    fn openai_discover_keeps_sdk_wire_first_and_adds_future_probe_version() {
        let mut response = json!({
            "result": {"supportedVersions": [mcp::SDK_WIRE_PROTOCOL, "2025-06-18"]}
        });
        augment_openai_discover(&mut response);
        assert_eq!(
            response["result"]["supportedVersions"][0],
            mcp::SDK_WIRE_PROTOCOL
        );
        assert!(
            response["result"]["supportedVersions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "2026-07-28")
        );
    }

    #[tokio::test]
    async fn router_matches_stateful_and_openai_session_contract() {
        let root = test_root("sessions");
        let app = candidate_router(test_state(&root));

        let response = app
            .clone()
            .oneshot(rpc_request(Method::GET, "/mcp", None, &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "stateful-test", "version": "1"}
            }
        });
        let response = app
            .clone()
            .oneshot(rpc_request(Method::POST, "/mcp", Some(initialize), &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(!session.is_empty());

        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(list.clone()),
                &[("mcp-session-id", session.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(list.clone()),
                &[
                    ("mcp-session-id", "stale"),
                    ("user-agent", "claude-connector/1.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"]["code"], -32001);

        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::GET,
                "/mcp",
                None,
                &[
                    ("mcp-session-id", "poison"),
                    ("user-agent", "openai-mcp/1.0.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        assert!(response.headers().get("mcp-session-id").is_none());
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "no-cache, no-transform"
        );
        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_millis(100), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            frame.into_data().unwrap(),
            Bytes::from_static(b": connected\n\n")
        );

        let openai_initialize = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "ChatGPT", "version": "1"}
            }
        });
        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(openai_initialize),
                &[
                    ("mcp-session-id", "poison"),
                    ("user-agent", "openai-mcp/1.0.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("mcp-session-id").is_none());

        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(list),
                &[
                    ("mcp-session-id", "poison"),
                    ("user-agent", "openai-mcp/1.0.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("mcp-session-id").is_none());

        let response = app
            .clone()
            .oneshot(rpc_request(
                Method::DELETE,
                "/mcp",
                None,
                &[("mcp-session-id", session.as_str())],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn router_restart_fences_stateful_sid_but_openai_stays_stateless() {
        let first_root = test_root("restart-first");
        let first = candidate_router(test_state(&first_root));
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "stateful-restart-test", "version": "1"}
            }
        });
        let response = first
            .clone()
            .oneshot(rpc_request(Method::POST, "/mcp", Some(initialize), &[]))
            .await
            .unwrap();
        let stale_session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_owned();
        drop(first);
        let _ = std::fs::remove_dir_all(first_root);

        let second_root = test_root("restart-second");
        let second = candidate_router(test_state(&second_root));
        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
        let response = second
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(list.clone()),
                &[
                    ("mcp-session-id", stale_session.as_str()),
                    ("user-agent", "claude-connector/1.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = second
            .clone()
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(list),
                &[
                    ("mcp-session-id", stale_session.as_str()),
                    ("user-agent", "openai-mcp/1.0.0"),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("mcp-session-id").is_none());

        drop(second);
        let _ = std::fs::remove_dir_all(second_root);
    }

    #[test]
    fn constant_time_compare_requires_equal_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
