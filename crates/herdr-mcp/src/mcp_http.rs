use crate::browser_control;
use crate::exec_sessions::ExecRegistry;
#[cfg(unix)]
use crate::extension_ipc::ExtensionIpcSocket;
use crate::herdr::HerdrClient;
use crate::mcp::{self, RuntimeContext};
use crate::paths::RuntimePaths;
use crate::prompt::PromptRegistry;
use crate::runtime_meta;
use crate::skill::SkillService;
use crate::state_cache::EventCache;
use crate::state_store::{ContinuityTurnInput, StateStore};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::convert::Infallible;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const RUNTIME_GENERATION_HEADER: &str = "x-herdr-runtime-generation";
const MAX_SESSIONS: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
const PUSH_CACHE_POLL: Duration = Duration::from_millis(250);
const MAX_MCP_ACTIVITY_RECORDS: usize = 2000;
const MAX_MCP_ACTIVITY_RESULTS: usize = 50;
const MAX_MCP_ACTIVITY_LOOKBACK_MS: u64 = 30 * 60_000;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

const SETTLED_AGENT_STATES: &[&str] = &["idle", "done", "blocked"];

#[derive(Clone, Default)]
struct SessionRegistry {
    inner: Arc<Mutex<HashMap<String, Instant>>>,
}

impl SessionRegistry {
    fn issue(&self, boot_id: &str) -> Option<String> {
        let id = new_session_id(boot_id);
        let Ok(mut sessions) = self.inner.lock() else {
            return None;
        };
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
        Some(id)
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

#[derive(Debug, Clone)]
struct McpActivityRecord {
    at_ms: u64,
    tool: String,
    call: Option<String>,
    user_agent: String,
    status: u16,
}

#[derive(Clone, Default)]
struct McpActivityRegistry {
    inner: Arc<Mutex<VecDeque<McpActivityRecord>>>,
}

impl McpActivityRegistry {
    fn record(&self, record: McpActivityRecord) {
        let Ok(mut records) = self.inner.lock() else {
            return;
        };
        records.push_back(record);
        while records.len() > MAX_MCP_ACTIVITY_RECORDS {
            records.pop_front();
        }
    }

    fn query(
        &self,
        since_ms: u64,
        until_ms: u64,
        ua_includes: &str,
    ) -> (usize, Vec<McpActivityRecord>) {
        let Ok(records) = self.inner.lock() else {
            return (0, vec![]);
        };
        let needle = ua_includes.to_ascii_lowercase();
        let hits = records
            .iter()
            .filter(|record| {
                record.at_ms >= since_ms
                    && record.at_ms <= until_ms
                    && (needle.is_empty()
                        || record
                            .user_agent
                            .to_ascii_lowercase()
                            .contains(needle.as_str()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let count = hits.len();
        let tools = hits
            .into_iter()
            .rev()
            .take(MAX_MCP_ACTIVITY_RESULTS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (count, tools)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().map(|records| records.len()).unwrap_or(0)
    }
}

#[derive(Clone)]
struct AppState {
    client: HerdrClient,
    cache: Arc<EventCache>,
    exec: ExecRegistry,
    prompt: PromptRegistry,
    skill: SkillService,
    state_store: Arc<Mutex<StateStore>>,
    sessions: SessionRegistry,
    activity: McpActivityRegistry,
    bearer_token: Arc<[u8]>,
    trusted_extension_ipc: bool,
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
    let state_store = Arc::new(Mutex::new(StateStore::open_in_dir(
        &exec_state_dir,
        "state",
    )?));
    let exec = ExecRegistry::new_with_client(exec_state_dir, Some(client.clone()))?;
    let prompt = PromptRegistry::with_store(state_store.clone());
    let skill = SkillService::new();
    crate::schema::prewarm_async();
    if !cache.wait_ready(Duration::from_secs(3)) {
        return Err(format!(
            "candidate event cache did not bootstrap: {}",
            cache
                .last_error()
                .unwrap_or_else(|| "unknown error".to_owned())
        ));
    }
    if !cache.wait_stream_connected(Duration::from_secs(2)) {
        return Err(format!(
            "candidate event cache did not connect events.subscribe: {}",
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
            state_store,
            sessions: SessionRegistry::default(),
            activity: McpActivityRegistry::default(),
            bearer_token: Arc::<[u8]>::from(token.into_bytes()),
            trusted_extension_ipc: false,
        };
        let app = candidate_router(state.clone());
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| format!("cannot bind candidate runtime to {address}: {error}"))?;

        #[cfg(unix)]
        let extension_ipc = if let Some(socket_path) = env::var_os("HERDR_EXTENSION_IPC_SOCKET") {
            let (extension_listener, guard) = ExtensionIpcSocket::bind(socket_path).await?;
            let mut extension_state = state.clone();
            extension_state.trusted_extension_ipc = true;
            let extension_app = candidate_router(extension_state);
            let task = tokio::spawn(async move {
                axum::serve(extension_listener, extension_app)
                    .await
                    .map_err(|error| format!("candidate extension IPC server failed: {error}"))
            });
            Some((task, guard))
        } else {
            None
        };

        #[cfg(not(unix))]
        if env::var_os("HERDR_EXTENSION_IPC_SOCKET").is_some() {
            return Err(
                "HERDR_EXTENSION_IPC_SOCKET is not supported on this platform yet".to_owned(),
            );
        }

        println!(
            "herdr-mcp candidate {} listening on http://{address}/mcp",
            env!("CARGO_PKG_VERSION")
        );
        let tcp_result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await;

        #[cfg(unix)]
        if let Some((task, guard)) = extension_ipc {
            task.abort();
            let _ = task.await;
            drop(guard);
        }

        tcp_result.map_err(|error| format!("candidate HTTP server failed: {error}"))?;
        Ok(ExitCode::SUCCESS)
    })
}

fn candidate_router(state: AppState) -> Router {
    Router::new()
        .route("/", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/mcp/", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route("/push/state", get(push_state))
        .route("/push/events", get(push_events))
        .route("/push/mcp-activity", get(push_mcp_activity))
        .route(
            "/extension/control/action",
            post(post_extension_control_action),
        )
        .route(
            "/extension/continuity/turn",
            post(post_extension_continuity_turn),
        )
        .route(
            "/extension/continuity/resolve",
            post(post_extension_continuity_resolve),
        )
        .route("/health", get(health))
        .with_state(state)
}

async fn post_extension_continuity_turn(State(state): State<AppState>, body: Bytes) -> Response {
    if !state.trusted_extension_ipc {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.len() > MAX_REQUEST_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let payload: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        _ => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": "invalid_json"}),
            );
        }
    };
    let required = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let Some(continuity_id) = required("continuity_id") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_id_required"}),
        );
    };
    let Some(conversation_id) = required("conversation_id") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "conversation_id_required"}),
        );
    };
    let Some(message_id) = required("message_id") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "message_id_required"}),
        );
    };
    let Some(role) = required("role") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "role_required"}),
        );
    };
    let Some(text) = required("text") else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "text_required"}),
        );
    };
    if continuity_id.len() > 160
        || conversation_id.len() > 256
        || message_id.len() > 256
        || text.len() > 64 * 1024
    {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_field_too_large"}),
        );
    }
    let observed_at = payload
        .get("observed_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(i64::MAX)
        });
    let workspace_id = payload.get("workspace_id").and_then(Value::as_str);
    let project_id = payload.get("project_id").and_then(Value::as_str);
    let title = payload.get("title").and_then(Value::as_str);
    let fingerprint = payload.get("fingerprint").and_then(Value::as_str);
    let inserted = match state.state_store.lock() {
        Ok(mut store) => store.append_continuity_turn(ContinuityTurnInput {
            continuity_id,
            conversation_id,
            workspace_id,
            project_id,
            title,
            message_id,
            role,
            text,
            fingerprint,
            observed_at,
        }),
        Err(_) => Err("continuity_store_lock_poisoned".to_owned()),
    };
    match inserted {
        Ok(inserted) => json_response(
            StatusCode::OK,
            &json!({"ok": true, "continuity_id": continuity_id, "inserted": inserted}),
        ),
        Err(error) => json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": error}),
        ),
    }
}

async fn post_extension_continuity_resolve(State(state): State<AppState>, body: Bytes) -> Response {
    if !state.trusted_extension_ipc {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.len() > MAX_REQUEST_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let payload: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        _ => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": "invalid_json"}),
            );
        }
    };
    let Some(continuity_id) = payload
        .get("continuity_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_id_required"}),
        );
    };
    if continuity_id.len() > 160 {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_field_too_large"}),
        );
    }
    let resolved = match state.state_store.lock() {
        Ok(store) => store.continuity_resume(continuity_id, 1),
        Err(_) => Err("continuity_store_lock_poisoned".to_owned()),
    };
    match resolved {
        Ok(Some(record)) => json_response(
            StatusCode::OK,
            &json!({
                "ok": true,
                "continuity_id": record.continuity_id,
                "status": record.status,
                "updated_at": record.updated_at
            }),
        ),
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            &json!({"ok": false, "error": "continuity_not_found"}),
        ),
        Err(error) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": error}),
        ),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Debug, Clone, Default)]
struct PushFilters {
    agent: Option<String>,
    pane: Option<String>,
    workspace: Option<String>,
}

struct PushStreamState {
    cache: Arc<EventCache>,
    cursor: u64,
    filters: PushFilters,
    statuses: HashMap<String, String>,
    first: bool,
    last_heartbeat: Instant,
}

async fn push_state(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !request_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    json_response(StatusCode::OK, &push_state_payload(&state.cache))
}

async fn post_extension_control_action(State(state): State<AppState>, body: Bytes) -> Response {
    if !state.trusted_extension_ipc {
        return json_response(
            StatusCode::FORBIDDEN,
            &json!({
                "ok": false,
                "outcome": "rejected",
                "delivery_phase": "not_submitted",
                "code": "trusted_extension_ipc_required",
            }),
        );
    }
    if body.len() > MAX_REQUEST_BYTES {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!({
                "ok": false,
                "outcome": "rejected",
                "delivery_phase": "not_submitted",
                "code": "request_too_large",
            }),
        );
    }
    let request = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "ok": false,
                    "outcome": "rejected",
                    "delivery_phase": "not_submitted",
                    "code": "invalid_request",
                    "message": "request body must be a JSON object",
                }),
            );
        }
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "ok": false,
                    "outcome": "rejected",
                    "delivery_phase": "not_submitted",
                    "code": "invalid_json",
                    "message": error.to_string(),
                }),
            );
        }
    };
    let blocking_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        browser_control::execute_action(
            &blocking_state.client,
            &blocking_state.cache,
            &blocking_state.prompt,
            &request,
        )
    })
    .await
    .unwrap_or_else(|error| {
        json!({
            "ok": false,
            "outcome": "uncertain",
            "delivery_phase": "uncertain",
            "code": "control_task_failed",
            "message": error.to_string(),
        })
    });
    json_response(StatusCode::OK, &result)
}

async fn push_events(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !request_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let filters = PushFilters {
        agent: query.get("agent").filter(|v| !v.is_empty()).cloned(),
        pane: query.get("pane").filter(|v| !v.is_empty()).cloned(),
        workspace: query.get("workspace").filter(|v| !v.is_empty()).cloned(),
    };
    push_events_response(state.cache, filters)
}

async fn push_mcp_activity(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !request_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let now_ms = epoch_ms();
    let since_raw = query.get("since").or_else(|| query.get("since_ms"));
    let since_ms = since_raw.and_then(|value| value.parse::<u64>().ok());
    let until_ms = query
        .get("until")
        .or_else(|| query.get("until_ms"))
        .map(|value| value.parse::<u64>().ok())
        .unwrap_or(Some(now_ms));
    let (Some(since_ms), Some(until_ms)) = (since_ms, until_ms) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "ok": false,
                "reason": "bad_window",
                "message": "query since (ms) and optional until (ms) required; until >= since",
            }),
        );
    };
    if until_ms < since_ms {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "ok": false,
                "reason": "bad_window",
                "message": "query since (ms) and optional until (ms) required; until >= since",
            }),
        );
    }
    let clipped_since = since_ms.max(now_ms.saturating_sub(MAX_MCP_ACTIVITY_LOOKBACK_MS));
    let ua_includes = query
        .get("ua")
        .or_else(|| query.get("ua_includes"))
        .map(String::as_str)
        .unwrap_or("openai-mcp");
    let (count, records) = state.activity.query(clipped_since, until_ms, ua_includes);
    let tools = records
        .into_iter()
        .map(|record| {
            json!({
                "at": iso_from_ms(record.at_ms),
                "tool": record.tool,
                "call": record.call,
                "ua": record.user_agent,
                "status": record.status,
            })
        })
        .collect::<Vec<_>>();
    json_response(
        StatusCode::OK,
        &json!({
            "ok": true,
            "since": iso_from_ms(clipped_since),
            "until": iso_from_ms(until_ms),
            "since_ms": clipped_since,
            "until_ms": until_ms,
            "ua_includes": if ua_includes.is_empty() { Value::Null } else { json!(ua_includes) },
            "count": count,
            "tools": tools,
        }),
    )
}

fn push_state_payload(cache: &EventCache) -> Value {
    let digest = cache.digest_since(u64::MAX);
    let snapshot = cache.snapshot();
    let agents = push_agent_views(&digest.agents);
    let raw_panes = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let panes = raw_panes
        .iter()
        .map(|pane| browser_control::pane_view(cache, pane))
        .collect::<Vec<_>>();
    json!({
        "boot_id": cache.boot_id(),
        "state_seq": digest.cursor,
        "server_time": iso_now(),
        "agents": agents,
        "workspaces": push_workspace_views(&digest.workspaces, &agents, &panes),
        "panes": panes,
    })
}

fn push_events_response(cache: Arc<EventCache>, filters: PushFilters) -> Response {
    let digest = cache.digest_since(u64::MAX);
    let agents = push_agent_views(&digest.agents);
    let statuses = agents
        .iter()
        .filter_map(|agent| {
            Some((
                agent.get("pane")?.as_str()?.to_owned(),
                agent.get("status")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    let state = PushStreamState {
        cache,
        cursor: digest.cursor,
        filters,
        statuses,
        first: true,
        last_heartbeat: Instant::now(),
    };
    let events = stream::unfold(state, |mut state| async move {
        if state.first {
            state.first = false;
            let digest = state.cache.digest_since(u64::MAX);
            let all_agents = push_agent_views(&digest.agents);
            let panes = state
                .cache
                .snapshot()
                .get("panes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let workspaces = push_workspace_views(&digest.workspaces, &all_agents, &panes);
            let agents = all_agents
                .into_iter()
                .filter(|agent| push_filter_matches(&state.filters, agent))
                .collect::<Vec<_>>();
            let hello = json!({
                "protocol": "herdr-mcp-push/v1",
                "server_time": iso_now(),
                "filters": push_filters_value(&state.filters),
                "agents": agents,
                "workspaces": workspaces,
            });
            let body = format!("retry: 2000\n\n{}", sse_event("hello", &hello));
            return Some((Ok::<Bytes, Infallible>(Bytes::from(body)), state));
        }

        loop {
            tokio::time::sleep(PUSH_CACHE_POLL).await;
            let digest = state.cache.digest_since(state.cursor);
            state.cursor = digest.cursor;
            let current_agents = push_agent_views(&digest.agents);
            let mut body = String::new();

            for event in &digest.events {
                if let Some((name, data)) = push_browser_lifecycle_event(event, &state.cache)
                    && push_filter_matches(&state.filters, &data)
                {
                    body.push_str(&sse_event(name, &data));
                }
                if let Some(agent) = push_agent_from_event(event, &digest.agents) {
                    append_push_transition(&mut state, &agent, &mut body);
                }
            }

            // Reconcile against the current cache view after every cursor read.
            // This heals an event-ring gap without opening a second daemon
            // subscription and without replaying settled history.
            for agent in &current_agents {
                append_push_transition(&mut state, agent, &mut body);
            }

            if state.last_heartbeat.elapsed() >= SSE_HEARTBEAT {
                body.push_str(": keepalive\n\n");
                state.last_heartbeat = Instant::now();
            }
            if !body.is_empty() {
                return Some((Ok::<Bytes, Infallible>(Bytes::from(body)), state));
            }
        }
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

fn push_browser_lifecycle_event(
    event: &Value,
    cache: &EventCache,
) -> Option<(&'static str, Value)> {
    let raw = event.get("type").and_then(Value::as_str)?;
    let normalized = raw.replace('.', "_");
    let kind = normalized.strip_suffix("_event").unwrap_or(&normalized);
    let at = event.get("at").cloned().unwrap_or_else(|| json!(iso_now()));

    match kind {
        "pane_created"
        | "pane_updated"
        | "pane_focused"
        | "pane_moved"
        | "pane_agent_detected"
        | "pane_agent_status_changed" => {
            let event_pane = event.get("pane")?;
            let pane_id = event
                .get("pane_id")
                .and_then(Value::as_str)
                .or_else(|| event_pane.get("pane_id").and_then(Value::as_str))?;
            // Herdr lifecycle events can be narrower than the authoritative
            // snapshot. Resolve the current pane before deriving target fencing
            // so an incremental update cannot temporarily publish a weaker or
            // mismatched target_revision.
            let snapshot = cache.snapshot();
            let live_pane = snapshot
                .get("panes")
                .and_then(Value::as_array)
                .and_then(|panes| {
                    panes
                        .iter()
                        .find(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(pane_id))
                })
                .unwrap_or(event_pane);
            let pane_data = browser_control::pane_view(cache, live_pane);
            let workspace = event
                .get("workspace_id")
                .and_then(Value::as_str)
                .or_else(|| pane_data.get("workspace_id").and_then(Value::as_str));
            Some((
                "pane_upsert",
                json!({
                    "pane": pane_id,
                    "pane_id": pane_id,
                    "workspace": workspace,
                    "pane_data": pane_data,
                    "at": at,
                }),
            ))
        }
        "pane_closed" | "pane_exited" => {
            let pane_id = event.get("pane_id").and_then(Value::as_str)?;
            Some((
                "pane_removed",
                json!({
                    "pane": pane_id,
                    "pane_id": pane_id,
                    "workspace": event.get("workspace_id").and_then(Value::as_str),
                    "at": at,
                }),
            ))
        }
        "workspace_created"
        | "workspace_updated"
        | "workspace_metadata_updated"
        | "workspace_renamed"
        | "workspace_moved"
        | "workspace_reordered" => {
            let workspace_data = event.get("workspace")?.clone();
            let workspace_id = event
                .get("workspace_id")
                .and_then(Value::as_str)
                .or_else(|| workspace_data.get("workspace_id").and_then(Value::as_str))?;
            Some((
                "workspace_upsert",
                json!({
                    "workspace": workspace_id,
                    "workspace_data": workspace_data,
                    "at": at,
                }),
            ))
        }
        "workspace_closed" => {
            let workspace_id = event.get("workspace_id").and_then(Value::as_str)?;
            Some((
                "workspace_removed",
                json!({
                    "workspace": workspace_id,
                    "at": at,
                }),
            ))
        }
        _ => None,
    }
}

fn append_push_transition(state: &mut PushStreamState, agent: &Value, body: &mut String) {
    let Some(pane) = agent.get("pane").and_then(Value::as_str) else {
        return;
    };
    let Some(status) = agent.get("status").and_then(Value::as_str) else {
        return;
    };
    let previous = state.statuses.get(pane).cloned();
    state.statuses.insert(pane.to_owned(), status.to_owned());
    if !push_filter_matches(&state.filters, agent) {
        return;
    }
    if status == "working" && previous.as_deref() != Some("working") {
        body.push_str(&sse_event("agent_working", agent));
    } else if SETTLED_AGENT_STATES.contains(&status) && previous.as_deref() == Some("working") {
        body.push_str(&sse_event("agent_settled", agent));
    }
}

fn push_agent_from_event(event: &Value, current_agents: &[Value]) -> Option<Value> {
    let pane_id = event.get("pane_id").and_then(Value::as_str).or_else(|| {
        event
            .get("pane")
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
    })?;
    let pane = event.get("pane").and_then(Value::as_object);
    let current = current_agents.iter().find(|agent| {
        agent.get("pane").and_then(Value::as_str) == Some(pane_id)
            || agent.get("pane_id").and_then(Value::as_str) == Some(pane_id)
    });
    let current_push = current.map(push_agent_view);
    let status = pane
        .and_then(|pane| pane.get("agent_status").or_else(|| pane.get("status")))
        .and_then(normalized_status)
        .or_else(|| {
            current_push
                .as_ref()
                .and_then(|agent| agent.get("status"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    let field = |name: &str| {
        pane.and_then(|pane| pane.get(name).cloned())
            .or_else(|| {
                current_push
                    .as_ref()
                    .and_then(|agent| agent.get(name).cloned())
            })
            .unwrap_or(Value::Null)
    };
    let workspace = event
        .get("workspace_id")
        .cloned()
        .or_else(|| pane.and_then(|pane| pane.get("workspace_id").cloned()))
        .or_else(|| {
            current_push
                .as_ref()
                .and_then(|agent| agent.get("workspace").cloned())
        })
        .unwrap_or(Value::Null);
    Some(json!({
        "agent": pane.and_then(|pane| pane.get("agent").cloned()).or_else(|| current_push.as_ref().and_then(|agent| agent.get("agent").cloned())).unwrap_or(Value::Null),
        "pane": pane_id,
        "status": status,
        "workspace": workspace,
        "cwd": field("cwd"),
        "terminal_title": pane.and_then(|pane| pane.get("terminal_title").cloned()).or_else(|| current_push.as_ref().and_then(|agent| agent.get("terminal_title").cloned())).unwrap_or(Value::Null),
        "seq": pane.and_then(|pane| pane.get("state_change_seq").cloned()).or_else(|| current_push.as_ref().and_then(|agent| agent.get("seq").cloned())).unwrap_or(Value::Null),
        "at": event.get("at").cloned().unwrap_or_else(|| json!(iso_now())),
    }))
}

fn push_agent_views(agents: &[Value]) -> Vec<Value> {
    agents.iter().map(push_agent_view).collect()
}

fn push_agent_view(agent: &Value) -> Value {
    json!({
        "name": agent.get("name").cloned().or_else(|| agent.get("agent").cloned()).unwrap_or(Value::Null),
        "agent": agent.get("name").cloned().or_else(|| agent.get("agent").cloned()).unwrap_or(Value::Null),
        "pane": agent.get("pane").cloned().or_else(|| agent.get("pane_id").cloned()).unwrap_or(Value::Null),
        "status": agent.get("status").and_then(normalized_status).map(Value::String).unwrap_or(Value::Null),
        "workspace": agent.get("workspace").cloned().or_else(|| agent.get("workspace_id").cloned()).unwrap_or(Value::Null),
        "cwd": agent.get("cwd").cloned().or_else(|| agent.get("foreground_cwd").cloned()).unwrap_or(Value::Null),
        "started_at": agent.get("started_at").cloned().unwrap_or(Value::Null),
        "last_activity_at": agent.get("last_activity_at").cloned().unwrap_or(Value::Null),
        "terminal_title": agent.get("terminal_title").cloned().unwrap_or(Value::Null),
        "seq": agent.get("seq").cloned().or_else(|| agent.get("state_change_seq").cloned()).unwrap_or(Value::Null),
    })
}

fn normalized_status(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn push_workspace_views(workspaces: &[Value], agents: &[Value], panes: &[Value]) -> Vec<Value> {
    let declared_project_keys = push_declared_project_keys(workspaces);
    workspaces
        .iter()
        .filter_map(|workspace| {
            let id = workspace
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| workspace.get("workspace_id").and_then(Value::as_str))?;
            let mut roots = Vec::<String>::new();
            if let Some(projects) = workspace.get("projects").and_then(Value::as_array) {
                for project in projects {
                    if let Some(root) = project.get("root").and_then(Value::as_str)
                        && !roots.iter().any(|known| known == root)
                    {
                        roots.push(root.to_owned());
                    }
                }
            }
            if roots.is_empty()
                && let Some(root) = workspace
                    .get("worktree")
                    .and_then(Value::as_object)
                    .and_then(|worktree| worktree.get("checkout_path"))
                    .and_then(Value::as_str)
                && !root.is_empty()
            {
                roots.push(root.to_owned());
            }
            if roots.is_empty()
                && let Some(cwd) = workspace.get("cwd").and_then(Value::as_str)
            {
                roots.push(cwd.to_owned());
            }
            if roots.is_empty() {
                for agent in agents {
                    if agent.get("workspace").and_then(Value::as_str) != Some(id) {
                        continue;
                    }
                    if let Some(cwd) = agent.get("cwd").and_then(Value::as_str)
                        && !roots.iter().any(|known| known == cwd)
                    {
                        roots.push(cwd.to_owned());
                    }
                }
            }
            if roots.is_empty() {
                for pane in panes {
                    if pane.get("workspace_id").and_then(Value::as_str) != Some(id) {
                        continue;
                    }
                    let cwd = pane
                        .get("cwd")
                        .and_then(Value::as_str)
                        .or_else(|| pane.get("foreground_cwd").and_then(Value::as_str));
                    if let Some(cwd) = cwd
                        && !roots.iter().any(|known| known == cwd)
                    {
                        roots.push(cwd.to_owned());
                    }
                }
            }
            let local_project_key = push_local_project_key(&roots, &declared_project_keys);
            Some(json!({
                "id": id,
                "label": workspace.get("label").cloned().unwrap_or(Value::Null),
                "roots": roots,
                "local_project_key": local_project_key,
            }))
        })
        .collect()
}

fn push_declared_project_keys(workspaces: &[Value]) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    for workspace in workspaces {
        let Some(worktree) = workspace.get("worktree").and_then(Value::as_object) else {
            continue;
        };
        let Some(repo_key) = worktree
            .get("repo_key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let project_key = format!("git:{repo_key}");
        for root in ["checkout_path", "repo_root"]
            .into_iter()
            .filter_map(|field| worktree.get(field).and_then(Value::as_str))
            .filter(|value| !value.is_empty())
        {
            keys.insert(root.to_owned(), project_key.clone());
        }
    }
    keys
}

fn push_local_project_key(
    roots: &[String],
    declared_project_keys: &HashMap<String, String>,
) -> Option<String> {
    let keys = roots
        .iter()
        .filter(|root| !root.is_empty())
        .map(|root| {
            declared_project_keys
                .get(root)
                .cloned()
                // Push-state projection is a liveness path. Never rediscover
                // Git identity from the filesystem here: a root may live in a
                // macOS privacy folder and the rotating runtime must not prompt
                // merely because the browser extension reconnects.
                .unwrap_or_else(|| format!("dir:{root}"))
        })
        .collect::<BTreeSet<_>>();
    (keys.len() == 1).then(|| keys.into_iter().next().unwrap())
}

fn push_filter_matches(filters: &PushFilters, data: &Value) -> bool {
    if let Some(workspace) = &filters.workspace
        && data.get("workspace").and_then(Value::as_str) != Some(workspace.as_str())
    {
        return false;
    }
    if let Some(pane) = &filters.pane
        && data.get("pane").and_then(Value::as_str) != Some(pane.as_str())
    {
        return false;
    }
    if let Some(agent) = &filters.agent {
        let want = agent
            .split_once(':')
            .map(|(_, suffix)| suffix)
            .unwrap_or(agent);
        let actual = data
            .get("agent")
            .or_else(|| data.get("name"))
            .and_then(Value::as_str);
        if actual != Some(agent.as_str()) && actual != Some(want) {
            return false;
        }
    }
    true
}

fn push_filters_value(filters: &PushFilters) -> Value {
    json!({
        "agent": filters.agent,
        "pane": filters.pane,
        "workspace": filters.workspace,
    })
}

fn sse_event(event: &str, data: &Value) -> String {
    let data = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_owned());
    format!("event: {event}\ndata: {data}\n\n")
}

fn iso_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn iso_from_ms(value: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

async fn health(State(state): State<AppState>) -> Response {
    let mut payload = runtime_meta::health_fields(&state.cache, Some(&state.exec));
    payload.insert("ok".to_owned(), json!(true));
    let payload = Value::Object(payload);
    json_response(StatusCode::OK, &payload)
}

async fn get_mcp(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !request_authorized(&state, &headers) {
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
    if !request_authorized(&state, &headers) {
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
    let started_at_ms = epoch_ms();
    if !request_authorized(&state, &headers) {
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

    let blocking_state = state.clone();
    let blocking_request = request.clone();
    let handled = tokio::task::spawn_blocking(move || {
        let context = RuntimeContext {
            client: &blocking_state.client,
            cache: &blocking_state.cache,
            exec: &blocking_state.exec,
            prompt: &blocking_state.prompt,
            skill: &blocking_state.skill,
            state_store: &blocking_state.state_store,
        };
        mcp::handle(&blocking_request, &context)
    })
    .await;
    let Some(mut response) = (match handled {
        Ok(response) => response,
        Err(_) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32603, "message": "Internal error"},
                    "id": request.get("id").cloned().unwrap_or(Value::Null)
                }),
            );
        }
    }) else {
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
        match state.sessions.issue(state.cache.boot_id()) {
            Some(session) => Some(session),
            None => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32603, "message": "transport session registry unavailable"},
                        "id": request.get("id").cloned().unwrap_or(Value::Null)
                    }),
                );
            }
        }
    } else {
        None
    };
    let mut http_response = if wants_sse(&headers, &request) {
        sse_response(&response, issued_session.as_deref())
    } else {
        json_response_with_session(StatusCode::OK, &response, issued_session.as_deref())
    };
    attach_runtime_generation_header(&mut http_response);
    if request.get("method").and_then(Value::as_str) == Some("tools/call")
        && let Some(tool) = request.pointer("/params/name").and_then(Value::as_str)
    {
        let call = (tool == "herdr_call")
            .then(|| {
                request
                    .pointer("/params/arguments/method")
                    .and_then(Value::as_str)
            })
            .flatten()
            .map(str::to_owned);
        let user_agent = headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("-")
            .to_owned();
        state.activity.record(McpActivityRecord {
            at_ms: started_at_ms,
            tool: tool.to_owned(),
            call,
            user_agent,
            status: http_response.status().as_u16(),
        });
    }
    http_response
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
    if !versions
        .iter()
        .any(|value| value == mcp::OPENAI_PROBE_PROTOCOL)
    {
        versions.push(json!(mcp::OPENAI_PROBE_PROTOCOL));
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

fn request_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    state.trusted_extension_ipc || authorized(headers, &state.bearer_token)
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

fn attach_runtime_generation_header(response: &mut Response) {
    let Some(generation) = env::var_os("HERDR_MCP_RUNTIME_GENERATION")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(&generation) {
        response
            .headers_mut()
            .insert(RUNTIME_GENERATION_HEADER, value);
    }
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

    fn control_request(body: Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/extension/control/action")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

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

    #[test]
    fn runtime_generation_header_reflects_service_process_identity() {
        let _guard = crate::test_env::lock();
        let previous = env::var_os("HERDR_MCP_RUNTIME_GENERATION");
        unsafe { env::set_var("HERDR_MCP_RUNTIME_GENERATION", "rust-generation-proof") };
        let mut response = json_response(StatusCode::OK, &json!({"ok": true}));
        attach_runtime_generation_header(&mut response);
        assert_eq!(
            response
                .headers()
                .get(RUNTIME_GENERATION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("rust-generation-proof")
        );
        unsafe {
            match previous {
                Some(value) => env::set_var("HERDR_MCP_RUNTIME_GENERATION", value),
                None => env::remove_var("HERDR_MCP_RUNTIME_GENERATION"),
            }
        }
    }

    #[test]
    fn browser_lifecycle_maps_pane_create_and_remove() {
        let cache = EventCache::from_snapshot_for_test(json!({"panes": []}));
        let created = json!({
            "type": "pane.created",
            "at": "2026-08-27T10:00:00Z",
            "workspace_id": "w1",
            "pane_id": "w1:p2",
            "pane": {
                "pane_id": "w1:p2",
                "workspace_id": "w1",
                "cwd": "/repo",
                "agent": null
            }
        });
        let (name, data) = push_browser_lifecycle_event(&created, &cache).expect("pane create");
        assert_eq!(name, "pane_upsert");
        assert_eq!(data["pane"], "w1:p2");
        assert_eq!(data["workspace"], "w1");
        assert_eq!(data["pane_data"]["cwd"], "/repo");

        let removed = json!({
            "type": "pane.closed",
            "at": "2026-08-27T10:00:01Z",
            "workspace_id": "w1",
            "pane_id": "w1:p2"
        });
        let (name, data) = push_browser_lifecycle_event(&removed, &cache).expect("pane remove");
        assert_eq!(name, "pane_removed");
        assert_eq!(data["pane_id"], "w1:p2");
        assert_eq!(data["workspace"], "w1");
    }

    #[test]
    fn browser_lifecycle_maps_workspace_create_and_remove() {
        let cache = EventCache::from_snapshot_for_test(json!({"panes": []}));
        let created = json!({
            "type": "workspace_created_event",
            "at": "2026-08-27T10:01:00Z",
            "workspace_id": "w2",
            "workspace": {
                "workspace_id": "w2",
                "label": "repo-two"
            }
        });
        let (name, data) =
            push_browser_lifecycle_event(&created, &cache).expect("workspace create");
        assert_eq!(name, "workspace_upsert");
        assert_eq!(data["workspace"], "w2");
        assert_eq!(data["workspace_data"]["label"], "repo-two");

        let removed = json!({
            "type": "workspace.closed",
            "at": "2026-08-27T10:01:01Z",
            "workspace_id": "w2"
        });
        let (name, data) =
            push_browser_lifecycle_event(&removed, &cache).expect("workspace remove");
        assert_eq!(name, "workspace_removed");
        assert_eq!(data["workspace"], "w2");
    }

    #[tokio::test]
    async fn extension_control_is_local_only_and_fences_stale_targets() {
        let root = test_root("browser-control-route");
        let snapshot = json!({
            "workspaces": [{"id": "w1", "label": "repo"}],
            "panes": [{
                "workspace_id": "w1",
                "pane_id": "w1:p1",
                "revision": 4,
                "agent": "codex",
                "agent_status": "working",
                "agent_session": {"agent": "codex", "kind": "id", "source": "herdr:codex", "value": "opaque-session"}
            }],
            "agents": []
        });
        let tcp_state = test_state_with_snapshot(&root.join("tcp"), snapshot.clone());
        let pane = tcp_state.cache.snapshot()["panes"][0].clone();
        let revision = browser_control::target_revision(&tcp_state.cache, &pane).unwrap();
        let request = json!({
            "action": "steer",
            "target": {"pane_id": "w1:p1", "target_revision": revision},
            "args": {"text": "keep compatibility"},
            "idempotency_key": "browser-control-test"
        });
        let response = candidate_router(tcp_state)
            .oneshot(control_request(request.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let mut extension_state = test_state_with_snapshot(&root.join("extension"), snapshot);
        extension_state.trusted_extension_ipc = true;
        let app = candidate_router(extension_state.clone());
        let response = app.clone().oneshot(control_request(request)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["outcome"], "session_not_resolved");
        assert_eq!(result["delivery_phase"], "not_submitted");
        assert_eq!(result["detail"]["prompt_fallback"], false);

        let stale = json!({
            "action": "agent_prompt",
            "target": {"pane_id": "w1:p1", "target_revision": "btr1_stale"},
            "args": {"text": "do work"},
            "idempotency_key": "browser-control-stale"
        });
        let response = app.oneshot(control_request(stale)).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["outcome"], "stale_target");
        assert_eq!(result["delivery_phase"], "not_submitted");
        assert!(
            result["target"]["target_revision"]
                .as_str()
                .unwrap()
                .starts_with("btr1_")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_state(root: &std::path::Path) -> AppState {
        test_state_with_snapshot(
            root,
            json!({
                "workspaces": [],
                "panes": [],
                "agents": []
            }),
        )
    }

    fn test_state_with_snapshot(root: &std::path::Path, snapshot: Value) -> AppState {
        AppState {
            client: HerdrClient::new(root.join("missing-herdr.sock")),
            cache: Arc::new(EventCache::from_snapshot_for_test(snapshot)),
            exec: ExecRegistry::new(root.join("exec")).unwrap(),
            prompt: PromptRegistry::new(),
            skill: SkillService::new(),
            state_store: Arc::new(Mutex::new(
                StateStore::open_in_dir(root, "state-test").unwrap(),
            )),
            sessions: SessionRegistry::default(),
            activity: McpActivityRegistry::default(),
            bearer_token: Arc::<[u8]>::from(b"test-token".to_vec()),
            trusted_extension_ipc: false,
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

    #[tokio::test]
    async fn continuity_turn_route_is_trusted_ipc_only_and_persists_idempotently() {
        let root = test_root("continuity-turn-route");
        let payload = json!({
            "continuity_id": "hc:test",
            "conversation_id": "conv-1",
            "workspace_id": "w19",
            "project_id": "project-1",
            "title": "continuity test",
            "message_id": "msg-1",
            "role": "user",
            "text": "continue",
            "fingerprint": "fp-1",
            "observed_at": 1234
        });
        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/extension/continuity/turn")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap()
        };

        let tcp = candidate_router(test_state(&root.join("tcp")));
        let response = tcp.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mut extension_state = test_state(&root.join("extension"));
        extension_state.trusted_extension_ipc = true;
        let store = extension_state.state_store.clone();
        let app = candidate_router(extension_state);
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["inserted"], true);

        let response = app.clone().oneshot(request()).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["inserted"], false);

        let resolve = Request::builder()
            .method(Method::POST)
            .uri("/extension/continuity/resolve")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"continuity_id": "hc:test"}).to_string()))
            .unwrap();
        let response = app.oneshot(resolve).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["continuity_id"], "hc:test");

        let resume = store
            .lock()
            .unwrap()
            .continuity_resume("hc:test", 32)
            .unwrap()
            .unwrap();
        assert_eq!(resume.turns.len(), 1);
        assert_eq!(resume.turns[0].text, "continue");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bearer_check_is_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert!(authorized(&headers, b"secret"));
        assert!(!authorized(&headers, b"other"));
        assert!(!authorized(&HeaderMap::new(), b"secret"));
    }

    #[tokio::test]
    async fn trusted_extension_ipc_bypasses_bearer_but_tcp_state_does_not() {
        let root = test_root("extension-ipc-auth");
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };

        let tcp = candidate_router(test_state(&root.join("tcp")));
        let tcp_response = tcp.oneshot(request()).await.unwrap();
        assert_eq!(tcp_response.status(), StatusCode::UNAUTHORIZED);

        let mut extension_state = test_state(&root.join("extension"));
        extension_state.trusted_extension_ipc = true;
        let extension = candidate_router(extension_state);
        let extension_response = extension.oneshot(request()).await.unwrap();
        assert_eq!(extension_response.status(), StatusCode::OK);
        let bytes = extension_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["result"]["tools"].as_array().unwrap().len(), 18);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn push_state_projects_cache_into_legacy_extension_shape() {
        let cache = EventCache::from_snapshot_for_test(json!({
            "workspaces": [{"workspace_id":"w1","label":"alpha","roots":["/tmp/alpha"]}],
            "panes": [{"pane_id":"w1:p1","workspace_id":"w1"}],
            "agents": [{
                "agent":"pi","pane_id":"w1:p1","agent_status":"working",
                "workspace_id":"w1","cwd":"/tmp/alpha","state_change_seq":7
            }]
        }));
        let payload = push_state_payload(&cache);
        assert_eq!(payload["agents"][0]["name"], "w1:p1");
        assert_eq!(payload["agents"][0]["pane"], "w1:p1");
        assert_eq!(payload["agents"][0]["status"], "working");
        assert_eq!(payload["agents"][0]["workspace"], "w1");
        assert_eq!(payload["agents"][0]["seq"], 7);
        assert_eq!(payload["workspaces"][0]["id"], "w1");
        assert_eq!(payload["workspaces"][0]["label"], "alpha");
        assert_eq!(payload["workspaces"][0]["roots"], json!(["/tmp/alpha"]));
        assert_eq!(
            payload["workspaces"][0]["local_project_key"],
            json!("dir:/tmp/alpha")
        );
        assert_eq!(payload["panes"][0]["pane_id"], "w1:p1");
    }

    #[test]
    fn push_state_uses_declared_worktree_identity_without_filesystem_discovery() {
        let checkout = "/Users/test/Documents/herdr-mcp-not-present";
        let repo_root = "/Users/test/Documents/herdr-mcp";
        let repo_key = "/Users/test/Documents/herdr-mcp/.git";
        let cache = EventCache::from_snapshot_for_test(json!({
            "workspaces": [{
                "workspace_id": "w1",
                "label": "alpha",
                "worktree": {
                    "checkout_path": checkout,
                    "is_linked_worktree": true,
                    "repo_key": repo_key,
                    "repo_root": repo_root
                }
            }],
            "panes": [],
            "agents": []
        }));
        let payload = push_state_payload(&cache);
        assert_eq!(payload["workspaces"][0]["roots"], json!([checkout]));
        assert_eq!(
            payload["workspaces"][0]["local_project_key"],
            json!(format!("git:{repo_key}"))
        );
    }

    #[test]
    fn push_local_project_key_never_rediscovers_git_from_disk() {
        let root = test_root("push-project-key-no-fs");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let root_text = root.to_string_lossy().into_owned();
        assert_eq!(
            push_local_project_key(std::slice::from_ref(&root_text), &HashMap::new()),
            Some(format!("dir:{root_text}"))
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn push_transition_emits_only_new_work_and_working_to_settled() {
        let cache = Arc::new(EventCache::from_snapshot_for_test(json!({})));
        let mut state = PushStreamState {
            cache,
            cursor: 0,
            filters: PushFilters::default(),
            statuses: HashMap::new(),
            first: false,
            last_heartbeat: Instant::now(),
        };
        let working = json!({
            "agent":"pi","pane":"w1:p1","status":"working","workspace":"w1"
        });
        let settled = json!({
            "agent":"pi","pane":"w1:p1","status":"done","workspace":"w1"
        });
        let mut body = String::new();
        append_push_transition(&mut state, &working, &mut body);
        assert!(body.contains("event: agent_working"));
        body.clear();
        append_push_transition(&mut state, &working, &mut body);
        assert!(
            body.is_empty(),
            "duplicate working state must not wake again"
        );
        append_push_transition(&mut state, &settled, &mut body);
        assert!(body.contains("event: agent_settled"));
        body.clear();
        append_push_transition(&mut state, &settled, &mut body);
        assert!(
            body.is_empty(),
            "duplicate settled state must not wake again"
        );
    }

    #[test]
    fn push_event_uses_event_specific_status_before_current_final_state() {
        let current = vec![json!({
            "name":"pi","pane":"w1:p1","status":"done","workspace":"w1"
        })];
        let event = json!({
            "at":"2026-08-25T10:00:00Z",
            "workspace_id":"w1",
            "pane_id":"w1:p1",
            "pane":{
                "agent":"pi","pane_id":"w1:p1","agent_status":"working",
                "workspace_id":"w1","state_change_seq":10
            }
        });
        let projected = push_agent_from_event(&event, &current).unwrap();
        assert_eq!(projected["status"], "working");
        assert_eq!(projected["seq"], 10);
    }

    #[tokio::test]
    async fn trusted_extension_push_routes_are_tokenless_but_tcp_remains_protected() {
        let root = test_root("push-auth");
        let snapshot = json!({
            "workspaces": [{"workspace_id":"w1","label":"alpha"}],
            "panes": [],
            "agents": []
        });
        let tcp = candidate_router(test_state_with_snapshot(
            &root.join("tcp"),
            snapshot.clone(),
        ));
        let tcp_request = Request::builder()
            .method(Method::GET)
            .uri("/push/state")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            tcp.oneshot(tcp_request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut extension_state = test_state_with_snapshot(&root.join("extension"), snapshot);
        extension_state.trusted_extension_ipc = true;
        let extension = candidate_router(extension_state);
        let state_request = Request::builder()
            .method(Method::GET)
            .uri("/push/state")
            .body(Body::empty())
            .unwrap();
        let response = extension.clone().oneshot(state_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let events_request = Request::builder()
            .method(Method::GET)
            .uri("/push/events")
            .body(Body::empty())
            .unwrap();
        let response = extension.oneshot(events_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        let mut body = response.into_body();
        let frame = body.frame().await.unwrap().unwrap();
        let bytes = frame.into_data().unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("retry: 2000"));
        assert!(text.contains("event: hello"));
        assert!(text.contains("herdr-mcp-push/v1"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mcp_activity_registry_is_bounded_and_returns_last_50_matches() {
        let activity = McpActivityRegistry::default();
        for index in 0..(MAX_MCP_ACTIVITY_RECORDS + 100) {
            activity.record(McpActivityRecord {
                at_ms: index as u64,
                tool: format!("tool-{index}"),
                call: None,
                user_agent: "openai-mcp/1.0".to_owned(),
                status: 200,
            });
        }
        assert_eq!(activity.len(), MAX_MCP_ACTIVITY_RECORDS);
        let (count, tools) = activity.query(0, u64::MAX, "openai-mcp");
        assert_eq!(count, MAX_MCP_ACTIVITY_RECORDS);
        assert_eq!(tools.len(), MAX_MCP_ACTIVITY_RESULTS);
        assert_eq!(tools.first().unwrap().tool, "tool-2050");
        assert_eq!(tools.last().unwrap().tool, "tool-2099");
        assert_eq!(activity.query(0, u64::MAX, "other-agent").0, 0);
    }

    #[tokio::test]
    async fn tools_call_records_mcp_activity_and_query_matches_node_contract() {
        let root = test_root("mcp-activity");
        let app = candidate_router(test_state(&root));
        let request = rpc_request(
            Method::POST,
            "/mcp",
            Some(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"herdr_methods","arguments":{}}
            })),
            &[("user-agent", "openai-mcp/1.0")],
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let query = Request::builder()
            .method(Method::GET)
            .uri("/push/mcp-activity?since=0&ua=openai-mcp")
            .header(AUTHORIZATION, "Bearer test-token")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(query).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["count"], 1);
        assert_eq!(payload["tools"][0]["tool"], "herdr_methods");
        assert_eq!(payload["tools"][0]["ua"], "openai-mcp/1.0");
        assert_eq!(payload["tools"][0]["status"], 200);
        assert!(payload["since_ms"].as_u64().unwrap() > 0);
        assert_eq!(payload["ua_includes"], "openai-mcp");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn herdr_call_local_skill_load_round_trips_through_rust_http_without_socket() {
        let root = test_root("local-skill-load");
        let app = candidate_router(test_state(&root));
        let request = rpc_request(
            Method::POST,
            "/mcp",
            Some(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{
                    "name":"herdr_call",
                    "arguments":{
                        "method":"herdr_mcp.skill.load",
                        "params":"{\"ids\":[\"files-search\"]}"
                    }
                }
            })),
            &[("user-agent", "openai-mcp/1.0")],
        );
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let text = payload["result"]["content"][0]["text"].as_str().unwrap();
        let local: Value = serde_json::from_str(text).unwrap();
        assert_eq!(local["ok"], true);
        assert_eq!(local["count"], 1);
        assert_eq!(local["skills"][0]["id"], "files-search");
        assert_eq!(local["skills"][0]["cache_hit"], false);
        assert_eq!(local["authorization"], "none");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn herdr_call_continuity_resume_round_trips_through_rust_http_without_socket() {
        let root = test_root("local-continuity-resume");
        let state = test_state(&root);
        {
            let mut store = state.state_store.lock().unwrap();
            store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:http-test",
                    conversation_id: "conv-http-test",
                    workspace_id: Some("w19"),
                    project_id: Some("project-http-test"),
                    title: Some("HTTP continuity test"),
                    message_id: "msg-http-user",
                    role: "user",
                    text: "continue from the persisted journal",
                    fingerprint: Some("fp-http-user"),
                    observed_at: 1700000000000,
                })
                .unwrap();
        }
        let app = candidate_router(state);
        let request = rpc_request(
            Method::POST,
            "/mcp",
            Some(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{
                    "name":"herdr_call",
                    "arguments":{
                        "method":"continuity.resume",
                        "params":"{\"continuity_id\":\"hc:http-test\"}"
                    }
                }
            })),
            &[("user-agent", "openai-mcp/1.0")],
        );
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let text = payload["result"]["content"][0]["text"].as_str().unwrap();
        let local: Value = serde_json::from_str(text).unwrap();
        assert_eq!(local["ok"], true);
        assert_eq!(local["continuity_id"], "hc:http-test");
        assert_eq!(local["turns"][0]["role"], "user");
        assert_eq!(
            local["turns"][0]["text"],
            "continue from the persisted journal"
        );
        assert!(
            local["instruction"]
                .as_str()
                .unwrap()
                .contains("Re-check live")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn herdr_call_continuity_search_round_trips_through_rust_http_without_socket() {
        let root = test_root("local-continuity-search");
        let state = test_state(&root);
        {
            let mut store = state.state_store.lock().unwrap();
            store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:http-search",
                    conversation_id: "conv-http-search",
                    workspace_id: Some("w19"),
                    project_id: Some("project-http-search"),
                    title: Some("HTTP continuity search"),
                    message_id: "msg-http-search-user",
                    role: "user",
                    text: "continue the persisted search chain",
                    fingerprint: Some("fp-http-search-user"),
                    observed_at: 1700000001000,
                })
                .unwrap();
        }
        let app = candidate_router(state);
        let request = rpc_request(
            Method::POST,
            "/mcp",
            Some(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{
                    "name":"herdr_call",
                    "arguments":{
                        "method":"continuity.search",
                        "params":"{\"workspace_id\":\"w19\"}"
                    }
                }
            })),
            &[("user-agent", "openai-mcp/1.0")],
        );
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let text = payload["result"]["content"][0]["text"].as_str().unwrap();
        let local: Value = serde_json::from_str(text).unwrap();
        assert_eq!(local["ok"], true);
        assert_eq!(local["resolution"], "unique_exact");
        assert_eq!(local["auto_resume_safe"], true);
        assert_eq!(local["confirmation_required"], false);
        assert_eq!(local["candidates"][0]["continuity_id"], "hc:http-search");
        assert_eq!(local["candidates"][0]["workspace_ids"][0], "w19");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn mcp_activity_bad_window_and_trusted_ipc_auth_match_push_contract() {
        let root = test_root("mcp-activity-auth");
        let tcp = candidate_router(test_state(&root.join("tcp")));
        let no_auth = Request::builder()
            .method(Method::GET)
            .uri("/push/mcp-activity?since=0")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            tcp.oneshot(no_auth).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut extension_state = test_state(&root.join("extension"));
        extension_state.trusted_extension_ipc = true;
        let extension = candidate_router(extension_state);
        let bad = Request::builder()
            .method(Method::GET)
            .uri("/push/mcp-activity?since=20&until=10")
            .body(Body::empty())
            .unwrap();
        let response = extension.clone().oneshot(bad).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let good = Request::builder()
            .method(Method::GET)
            .uri("/push/mcp-activity?since=0")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            extension.oneshot(good).await.unwrap().status(),
            StatusCode::OK
        );
        std::fs::remove_dir_all(root).ok();
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
        let first = sessions.issue("boot-test").unwrap();
        assert!(sessions.contains_touch(&first));
        assert!(sessions.remove(&first));
        assert!(!sessions.contains_touch(&first));

        for _ in 0..=MAX_SESSIONS {
            sessions.issue("boot-test").unwrap();
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
                .any(|value| value == mcp::OPENAI_PROBE_PROTOCOL)
        );
    }

    #[tokio::test]
    async fn openai_discover_accepts_future_protocol_version_header() {
        let root = test_root("openai-discover-header");
        let app = candidate_router(test_state(&root));
        let discover = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "server/discover",
            "params": {}
        });
        let response = app
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(discover),
                &[
                    ("user-agent", "openai-mcp/1.0.0"),
                    ("mcp-protocol-version", mcp::OPENAI_PROBE_PROTOCOL),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            payload["result"]["supportedVersions"][0],
            mcp::SDK_WIRE_PROTOCOL
        );
        assert!(
            payload["result"]["supportedVersions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == mcp::OPENAI_PROBE_PROTOCOL)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn openai_tools_call_accepts_future_protocol_version_header() {
        let root = test_root("openai-call-header");
        let app = candidate_router(test_state(&root));
        let call = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": "herdr_methods", "arguments": {"query": "ping"}}
        });
        let response = app
            .oneshot(rpc_request(
                Method::POST,
                "/mcp",
                Some(call),
                &[
                    ("user-agent", "openai-mcp/1.0.0"),
                    ("mcp-protocol-version", mcp::OPENAI_PROBE_PROTOCOL),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert!(payload.get("result").is_some());
        assert!(payload.get("error").is_none());
        let _ = std::fs::remove_dir_all(root);
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

    #[tokio::test]
    async fn openai_sessionless_stress_does_not_allocate_transport_sessions() {
        let root = test_root("openai-stress");
        let state = test_state(&root);
        let sessions = state.sessions.clone();
        let app = candidate_router(state);
        for id in 0..100 {
            let list = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/list",
                "params": {}
            });
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
        }
        assert_eq!(sessions.len(), 0);

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn constant_time_compare_requires_equal_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
