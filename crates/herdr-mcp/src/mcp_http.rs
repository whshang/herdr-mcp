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
use crate::state_store::StateStore;
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
use std::collections::{HashMap, VecDeque};
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
    let exec = ExecRegistry::new(exec_state_dir)?;
    let prompt = PromptRegistry::with_store(state_store);
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
        .route("/health", get(health))
        .with_state(state)
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
    let panes = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
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
                let Some(agent) = push_agent_from_event(event, &digest.agents) else {
                    continue;
                };
                append_push_transition(&mut state, &agent, &mut body);
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
            Some(json!({
                "id": id,
                "label": workspace.get("label").cloned().unwrap_or(Value::Null),
                "roots": roots,
            }))
        })
        .collect()
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
    let http_response = if wants_sse(&headers, &request) {
        sse_response(&response, issued_session.as_deref())
    } else {
        json_response_with_session(StatusCode::OK, &response, issued_session.as_deref())
    };
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
        assert_eq!(payload["agents"][0]["name"], "pi");
        assert_eq!(payload["agents"][0]["pane"], "w1:p1");
        assert_eq!(payload["agents"][0]["status"], "working");
        assert_eq!(payload["agents"][0]["workspace"], "w1");
        assert_eq!(payload["agents"][0]["seq"], 7);
        assert_eq!(payload["workspaces"][0]["id"], "w1");
        assert_eq!(payload["workspaces"][0]["label"], "alpha");
        assert_eq!(payload["workspaces"][0]["roots"], json!(["/tmp/alpha"]));
        assert_eq!(payload["panes"][0]["pane_id"], "w1:p1");
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
