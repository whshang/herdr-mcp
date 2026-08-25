use crate::exec_sessions::ExecRegistry;
use crate::herdr::HerdrClient;
use crate::mcp::{self, RuntimeContext};
use crate::paths::RuntimePaths;
use crate::runtime_meta;
use crate::state_cache::EventCache;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
struct AppState {
    client: HerdrClient,
    cache: Arc<EventCache>,
    exec: ExecRegistry,
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
            bearer_token: Arc::<[u8]>::from(token.into_bytes()),
        };
        let app = Router::new()
            .route("/", post(post_mcp).get(get_mcp))
            .route("/mcp", post(post_mcp).get(get_mcp))
            .route("/health", get(health))
            .with_state(state);
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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health(State(state): State<AppState>) -> Response {
    let mut payload = runtime_meta::health_fields(&state.cache, Some(&state.exec));
    payload.insert("ok".to_owned(), json!(true));
    let payload = Value::Object(payload);
    json_response(StatusCode::OK, &payload)
}

async fn get_mcp() -> Response {
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &json!({
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": "Method not allowed."},
            "id": null
        }),
    )
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

    let context = RuntimeContext {
        client: &state.client,
        cache: &state.cache,
        exec: &state.exec,
    };
    let Some(response) = mcp::handle(&request, &context) else {
        return StatusCode::ACCEPTED.into_response();
    };
    if wants_sse(&headers, &request) {
        sse_response(&response)
    } else {
        json_response(StatusCode::OK, &response)
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
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn sse_response(value: &Value) -> Response {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned());
    let body = format!("event: message\ndata: {json}\n\n");
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header("cache-control", HeaderValue::from_static("no-cache"))
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn constant_time_compare_requires_equal_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
