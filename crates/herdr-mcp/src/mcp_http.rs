use crate::browser_control;
use crate::exec_sessions::ExecRegistry;
#[cfg(unix)]
use crate::extension_ipc::ExtensionIpcSocket;
use crate::herdr::HerdrClient;
use crate::mcp::{
    self, BrowserActuator, BrowserCallerGrant, BrowserPostconditionEvidence, RuntimeContext,
};
use crate::paths::RuntimePaths;
use crate::prompt::PromptRegistry;
use crate::runtime_meta;
use crate::skill::SkillService;
use crate::state_cache::EventCache;
use crate::state_store::{
    BrowserEndpointConsentInput, BrowserEndpointRegistrationInput, BrowserProviderObservationInput,
    BrowserResourceObservationInput, ContinuityTurnInput, StateStore,
};
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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_BROWSER_REGISTRY_REQUEST_BYTES: usize = 64 * 1024;
const MAX_BROWSER_ACTUATION_RESULT_BYTES: usize = 64 * 1024;
const BROWSER_ACTUATION_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_EXTENSION_LIVE_WINDOW: Duration = Duration::from_secs(2);
const RUNTIME_GENERATION_HEADER: &str = "x-herdr-runtime-generation";
const EDGE_WEBCHAT_CONTROL_GRANTS_HEADER: &str = "x-herdr-edge-webchat-control-grants";
const EDGE_EXPECTED_RUNTIME_GENERATION_HEADER: &str = "x-herdr-edge-expected-runtime-generation";
const MAX_EDGE_WEBCHAT_CONTROL_GRANTS_HEADER_BYTES: usize = 8 * 1024;
const MAX_SESSIONS: usize = 1024;
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
const PUSH_CACHE_POLL: Duration = Duration::from_millis(250);
const MAX_MCP_ACTIVITY_RECORDS: usize = 2000;
const MAX_MCP_ACTIVITY_RESULTS: usize = 50;
const MAX_MCP_ACTIVITY_LOOKBACK_MS: u64 = 30 * 60_000;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);
static NEXT_BROWSER_ACTUATION: AtomicU64 = AtomicU64::new(0);

const SETTLED_AGENT_STATES: &[&str] = &["idle", "done", "blocked"];

#[derive(Clone, Default)]
struct BrowserActuationBroker {
    inner: Arc<(Mutex<BrowserActuationState>, Condvar)>,
}

#[derive(Default)]
struct BrowserActuationState {
    queued: VecDeque<Value>,
    pending: BTreeSet<String>,
    completions: HashMap<String, BrowserPostconditionEvidence>,
    last_extension_poll: Option<Instant>,
}

impl BrowserActuationBroker {
    fn take_next_for_extension(&self) -> Option<Value> {
        let Ok(mut state) = self.inner.0.lock() else {
            return None;
        };
        state.last_extension_poll = Some(Instant::now());
        state.queued.pop_front()
    }

    fn complete(
        &self,
        actuation_id: &str,
        evidence: BrowserPostconditionEvidence,
    ) -> Result<(), String> {
        let (lock, ready) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
        if !state.pending.contains(actuation_id) {
            return Err("browser_actuation_not_pending".to_owned());
        }
        state.completions.insert(actuation_id.to_owned(), evidence);
        ready.notify_all();
        Ok(())
    }

    fn extension_live(state: &BrowserActuationState) -> bool {
        state
            .last_extension_poll
            .is_some_and(|seen| seen.elapsed() <= BROWSER_EXTENSION_LIVE_WINDOW)
    }
}

impl BrowserActuator for BrowserActuationBroker {
    fn actuate(
        &self,
        operation: &str,
        params: &Value,
        expected_generation: i64,
    ) -> Result<BrowserPostconditionEvidence, String> {
        let actuation_id = format!(
            "ba_{:016x}",
            NEXT_BROWSER_ACTUATION.fetch_add(1, Ordering::Relaxed)
        );
        let (lock, ready) = &*self.inner;
        let mut state = lock
            .lock()
            .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
        if !Self::extension_live(&state) {
            return Ok(BrowserPostconditionEvidence {
                observed_generation: expected_generation,
                command_accepted: false,
                browser_online: false,
                resource_available: true,
                rejected: false,
                stable_resource_ref_observed: false,
                lifecycle_observed: false,
                canonical_url_observed: false,
                accepted_message_observed: false,
                message_baseline_advanced: false,
                reasoning_effort_readback: None,
                required_apps_readback: Vec::new(),
                generation_owner: None,
                generation_status_observed: false,
                generation_stopped: false,
            });
        }
        state.pending.insert(actuation_id.clone());
        state.queued.push_back(json!({
            "protocol": "herdr-browser-actuation/v1",
            "actuation_id": actuation_id,
            "operation": operation,
            "expected_generation": expected_generation,
            "params": params,
        }));
        ready.notify_all();

        let deadline = Instant::now() + BROWSER_ACTUATION_TIMEOUT;
        loop {
            if let Some(evidence) = state.completions.remove(&actuation_id) {
                state.pending.remove(&actuation_id);
                return Ok(evidence);
            }
            let now = Instant::now();
            if now >= deadline {
                state.pending.remove(&actuation_id);
                state.queued.retain(|command| {
                    command.get("actuation_id").and_then(Value::as_str)
                        != Some(actuation_id.as_str())
                });
                // Once a command may have crossed the SSE boundary, a missing
                // acknowledgement is delivery-uncertain and must never invite replay.
                return Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: false,
                    lifecycle_observed: false,
                    canonical_url_observed: false,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: None,
                    generation_status_observed: false,
                    generation_stopped: false,
                });
            }
            let timeout = deadline.saturating_duration_since(now);
            let waited = ready
                .wait_timeout(state, timeout)
                .map_err(|_| "browser_actuation_broker_unavailable".to_owned())?;
            state = waited.0;
        }
    }
}

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
    local_device_id: Option<String>,
    browser_actuation: BrowserActuationBroker,
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
    let local_device_id =
        crate::config::Config::load_for_instance(&paths.config_file, &paths.instance)?
            .edge_device_id;
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
            local_device_id,
            browser_actuation: BrowserActuationBroker::default(),
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
        .route("/extension/fleet", get(get_extension_fleet))
        .route(
            "/extension/browser/registry",
            post(post_extension_browser_registry),
        )
        .route(
            "/extension/browser/actuation",
            post(post_extension_browser_actuation),
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

async fn post_extension_browser_registry(State(state): State<AppState>, body: Bytes) -> Response {
    if !state.trusted_extension_ipc {
        return browser_registry_http_error(
            StatusCode::FORBIDDEN,
            "trusted_extension_ipc_required",
        );
    }
    if body.len() > MAX_BROWSER_REGISTRY_REQUEST_BYTES {
        return browser_registry_http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "browser_registry_request_too_large",
        );
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(Value::Object(object)) => Value::Object(object),
        _ => {
            return browser_registry_http_error(
                StatusCode::BAD_REQUEST,
                "browser_registry_invalid_json",
            );
        }
    };
    let Some(operation) = payload.get("operation").and_then(Value::as_str) else {
        return browser_registry_http_error(
            StatusCode::BAD_REQUEST,
            "browser_registry_operation_required",
        );
    };
    let observed_at = match payload.get("observed_at") {
        None => i64::try_from(epoch_ms()).unwrap_or(i64::MAX),
        Some(value) => match value.as_i64() {
            Some(value) if value >= 0 => value,
            _ => {
                return browser_registry_http_error(
                    StatusCode::BAD_REQUEST,
                    "browser_observed_at_invalid",
                );
            }
        },
    };
    let result = match operation {
        "endpoint.register" => extension_browser_endpoint_register(&state, &payload, observed_at),
        "endpoint.consent" => extension_browser_endpoint_consent(&state, &payload, observed_at),
        "provider.observe" => extension_browser_provider_observe(&state, &payload, observed_at),
        "resource.observe" => extension_browser_resource_observe(&state, &payload, observed_at),
        _ => Err("browser_registry_operation_unknown".to_owned()),
    };
    match result {
        Ok(value) => json_response(StatusCode::OK, &value),
        Err(code) => browser_registry_http_store_error(&code),
    }
}

async fn post_extension_browser_actuation(State(state): State<AppState>, body: Bytes) -> Response {
    if !state.trusted_extension_ipc {
        return browser_registry_http_error(
            StatusCode::FORBIDDEN,
            "trusted_extension_ipc_required",
        );
    }
    if body.len() > MAX_BROWSER_ACTUATION_RESULT_BYTES {
        return browser_registry_http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "browser_actuation_result_too_large",
        );
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(Value::Object(object)) => Value::Object(object),
        _ => {
            return browser_registry_http_error(
                StatusCode::BAD_REQUEST,
                "browser_actuation_result_invalid_json",
            );
        }
    };
    if let Err(code) = browser_registry_allow_fields(
        &payload,
        &[
            "actuation_id",
            "observed_generation",
            "command_accepted",
            "browser_online",
            "resource_available",
            "rejected",
            "stable_resource_ref_observed",
            "lifecycle_observed",
            "canonical_url_observed",
            "accepted_message_observed",
            "message_baseline_advanced",
            "reasoning_effort_readback",
            "required_apps_readback",
            "generation_owner",
            "generation_status_observed",
            "generation_stopped",
        ],
    ) {
        return browser_registry_http_store_error(&code);
    }
    let result = (|| -> Result<(), String> {
        let actuation_id = browser_registry_string(&payload, "actuation_id", 96)?;
        if !actuation_id.starts_with("ba_") {
            return Err("browser_actuation_id_invalid".to_owned());
        }
        let observed_generation = browser_registry_positive_i64(&payload, "observed_generation")?;
        let reasoning_effort_readback = match payload.get("reasoning_effort_readback") {
            None | Some(Value::Null) => None,
            Some(Value::String(value))
                if matches!(value.as_str(), "economy" | "balanced" | "thorough") =>
            {
                Some(value.clone())
            }
            _ => return Err("browser_reasoning_effort_readback_invalid".to_owned()),
        };
        let required_apps_readback = match payload.get("required_apps_readback") {
            Some(Value::Array(items)) if items.len() <= 32 => {
                let mut apps = Vec::with_capacity(items.len());
                for item in items {
                    let Some(app) = item.as_str() else {
                        return Err("browser_required_apps_readback_invalid".to_owned());
                    };
                    if app.is_empty()
                        || app.len() > 64
                        || app != app.trim()
                        || app.chars().any(char::is_control)
                    {
                        return Err("browser_required_apps_readback_invalid".to_owned());
                    }
                    apps.push(app.to_owned());
                }
                apps.sort();
                apps.dedup();
                apps
            }
            _ => return Err("browser_required_apps_readback_invalid".to_owned()),
        };
        let generation_owner = match payload.get("generation_owner") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_i64() {
                Some(value) if value >= 1 => Some(value),
                _ => return Err("browser_generation_owner_invalid".to_owned()),
            },
        };
        state.browser_actuation.complete(
            actuation_id,
            BrowserPostconditionEvidence {
                observed_generation,
                command_accepted: browser_registry_bool(&payload, "command_accepted")?,
                browser_online: browser_registry_bool(&payload, "browser_online")?,
                resource_available: browser_registry_bool(&payload, "resource_available")?,
                rejected: browser_registry_bool(&payload, "rejected")?,
                stable_resource_ref_observed: browser_registry_bool(
                    &payload,
                    "stable_resource_ref_observed",
                )?,
                lifecycle_observed: browser_registry_bool(&payload, "lifecycle_observed")?,
                canonical_url_observed: browser_registry_bool(&payload, "canonical_url_observed")?,
                accepted_message_observed: browser_registry_bool(
                    &payload,
                    "accepted_message_observed",
                )?,
                message_baseline_advanced: browser_registry_bool(
                    &payload,
                    "message_baseline_advanced",
                )?,
                reasoning_effort_readback,
                required_apps_readback,
                generation_owner,
                generation_status_observed: browser_registry_bool(
                    &payload,
                    "generation_status_observed",
                )?,
                generation_stopped: browser_registry_bool(&payload, "generation_stopped")?,
            },
        )
    })();
    match result {
        Ok(()) => json_response(StatusCode::OK, &json!({"ok": true})),
        Err(code) => browser_registry_http_store_error(&code),
    }
}

fn extension_browser_endpoint_register(
    state: &AppState,
    payload: &Value,
    observed_at: i64,
) -> Result<Value, String> {
    browser_registry_allow_fields(
        payload,
        &[
            "operation",
            "profile_seed",
            "browser_family",
            "extension_version",
            "observed_at",
        ],
    )?;
    let device_id = state
        .local_device_id
        .as_deref()
        .ok_or_else(|| "browser_device_identity_unavailable".to_owned())?;
    let profile_seed = browser_registry_string(payload, "profile_seed", 256)?;
    let browser_family = browser_registry_string(payload, "browser_family", 32)?;
    let extension_version = browser_registry_string(payload, "extension_version", 64)?;
    let mut store = state
        .state_store
        .lock()
        .map_err(|_| "browser_registry_store_unavailable".to_owned())?;
    let endpoint = store.register_browser_endpoint(BrowserEndpointRegistrationInput {
        device_id,
        profile_seed,
        browser_family,
        extension_version,
        observed_at,
    })?;
    Ok(json!({
        "ok": true,
        "endpoint": browser_endpoint_http_json(endpoint),
    }))
}

fn browser_registry_authoritative_endpoint_ref(
    state: &AppState,
    payload: &Value,
) -> Result<String, String> {
    let profile_seed = browser_registry_string(payload, "profile_seed", 256)?;
    let device_id = state
        .local_device_id
        .as_deref()
        .ok_or_else(|| "browser_device_identity_unavailable".to_owned())?;
    let authoritative_ref =
        crate::state_store::derive_browser_endpoint_ref(device_id, profile_seed)?;
    if browser_registry_optional_string(payload, "endpoint_ref", 96)?
        .is_some_and(|caller_ref| caller_ref != authoritative_ref)
    {
        return Err("browser_endpoint_ref_mismatch".to_owned());
    }
    Ok(authoritative_ref)
}

fn extension_browser_endpoint_consent(
    state: &AppState,
    payload: &Value,
    observed_at: i64,
) -> Result<Value, String> {
    browser_registry_allow_fields(
        payload,
        &[
            "operation",
            "profile_seed",
            "endpoint_ref",
            "expected_consent_revision",
            "expected_revision",
            "webchat_control_allowed",
            "tool_bridge_allowed",
            "tool_bridge_mutation_allowed",
            "observed_at",
        ],
    )?;
    let endpoint_ref = browser_registry_authoritative_endpoint_ref(state, payload)?;
    let expected_revision = match payload
        .get("expected_consent_revision")
        .or_else(|| payload.get("expected_revision"))
        .and_then(Value::as_i64)
    {
        Some(value) if value >= 0 => value,
        _ => return Err("browser_expected_consent_revision_required".to_owned()),
    };
    let webchat_control_allowed = browser_registry_bool(payload, "webchat_control_allowed")?;
    let tool_bridge_allowed = browser_registry_bool(payload, "tool_bridge_allowed")?;
    let tool_bridge_mutation_allowed =
        browser_registry_bool(payload, "tool_bridge_mutation_allowed")?;
    let mut store = state
        .state_store
        .lock()
        .map_err(|_| "browser_registry_store_unavailable".to_owned())?;
    let endpoint = store.set_browser_endpoint_consent(BrowserEndpointConsentInput {
        endpoint_ref: &endpoint_ref,
        expected_revision,
        webchat_control_allowed,
        tool_bridge_allowed,
        tool_bridge_mutation_allowed,
        observed_at,
    })?;
    Ok(json!({
        "ok": true,
        "endpoint": browser_endpoint_http_json(endpoint),
    }))
}

fn extension_browser_provider_observe(
    state: &AppState,
    payload: &Value,
    observed_at: i64,
) -> Result<Value, String> {
    browser_registry_allow_fields(
        payload,
        &[
            "operation",
            "profile_seed",
            "endpoint_ref",
            "provider",
            "adapter_protocol_version",
            "observation_generation",
            "capabilities",
            "observed_at",
        ],
    )?;
    let endpoint_ref = browser_registry_authoritative_endpoint_ref(state, payload)?;
    let provider = browser_registry_string(payload, "provider", 32)?;
    let adapter_protocol_version =
        browser_registry_positive_i64(payload, "adapter_protocol_version")?;
    let observation_generation = browser_registry_positive_i64(payload, "observation_generation")?;
    let capabilities = payload
        .get("capabilities")
        .filter(|value| value.is_object())
        .ok_or_else(|| "browser_capabilities_invalid".to_owned())?;
    let capabilities_json = serde_json::to_string(capabilities)
        .map_err(|_| "browser_capabilities_invalid".to_owned())?;
    let mut store = state
        .state_store
        .lock()
        .map_err(|_| "browser_registry_store_unavailable".to_owned())?;
    let provider_state = store.observe_browser_provider(BrowserProviderObservationInput {
        endpoint_ref: &endpoint_ref,
        provider,
        adapter_protocol_version,
        observation_generation,
        capabilities_json: &capabilities_json,
        observed_at,
    })?;
    Ok(json!({
        "ok": true,
        "provider_state": {
            "endpoint_ref": provider_state.endpoint_ref,
            "provider": provider_state.provider,
            "adapter_protocol_version": provider_state.adapter_protocol_version,
            "observation_generation": provider_state.observation_generation,
            "capabilities": serde_json::from_str::<Value>(&provider_state.capabilities_json)
                .unwrap_or_else(|_| json!({})),
            "observed_at": provider_state.observed_at,
        }
    }))
}

fn extension_browser_resource_observe(
    state: &AppState,
    payload: &Value,
    observed_at: i64,
) -> Result<Value, String> {
    browser_registry_allow_fields(
        payload,
        &[
            "operation",
            "profile_seed",
            "endpoint_ref",
            "provider",
            "kind",
            "parent_ref",
            "native_identity",
            "display_label",
            "observation_generation",
            "observed_at",
        ],
    )?;
    let endpoint_ref = browser_registry_authoritative_endpoint_ref(state, payload)?;
    let provider = browser_registry_string(payload, "provider", 32)?;
    let kind = browser_registry_string(payload, "kind", 16)?;
    let parent_ref = browser_registry_optional_string(payload, "parent_ref", 96)?;
    let native_identity = browser_registry_string(payload, "native_identity", 1024)?;
    let display_label = browser_registry_optional_string(payload, "display_label", 256)?;
    let observation_generation = browser_registry_positive_i64(payload, "observation_generation")?;
    let mut store = state
        .state_store
        .lock()
        .map_err(|_| "browser_registry_store_unavailable".to_owned())?;
    let resource = store.observe_browser_resource(BrowserResourceObservationInput {
        endpoint_ref: &endpoint_ref,
        provider,
        kind,
        parent_ref,
        native_identity,
        display_label,
        observation_generation,
        observed_at,
    })?;
    Ok(json!({
        "ok": true,
        "resource": browser_resource_http_json(resource),
    }))
}

fn browser_registry_allow_fields(payload: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "browser_registry_invalid_json".to_owned())?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("browser_registry_params_invalid".to_owned());
    }
    Ok(())
}

fn browser_registry_string<'a>(
    payload: &'a Value,
    field: &str,
    max_bytes: usize,
) -> Result<&'a str, String> {
    let value = payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("browser_{field}_required"))?;
    if value.is_empty()
        || value.len() > max_bytes
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(format!("browser_{field}_invalid"));
    }
    Ok(value)
}

fn browser_registry_optional_string<'a>(
    payload: &'a Value,
    field: &str,
    max_bytes: usize,
) -> Result<Option<&'a str>, String> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.is_empty()
                || value.len() > max_bytes
                || value != value.trim()
                || value.chars().any(char::is_control)
            {
                return Err(format!("browser_{field}_invalid"));
            }
            Ok(Some(value))
        }
        Some(_) => Err(format!("browser_{field}_invalid")),
    }
}

fn browser_registry_bool(payload: &Value, field: &str) -> Result<bool, String> {
    payload
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("browser_{field}_invalid"))
}

fn browser_registry_positive_i64(payload: &Value, field: &str) -> Result<i64, String> {
    match payload.get(field).and_then(Value::as_i64) {
        Some(value) if value >= 1 => Ok(value),
        _ => Err(format!("browser_{field}_invalid")),
    }
}

fn browser_registry_http_store_error(code: &str) -> Response {
    let status = if code.ends_with("_not_found") {
        StatusCode::NOT_FOUND
    } else if code.contains("stale_")
        || code.contains("_ambiguous")
        || code.contains("_conflict")
        || code.contains("_mismatch")
        || code.contains("_hierarchy_")
        || code.contains("_parent_")
        || code == "browser_device_identity_unavailable"
    {
        StatusCode::CONFLICT
    } else if code == "browser_registry_store_unavailable" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_REQUEST
    };
    browser_registry_http_error(status, code)
}

fn browser_registry_http_error(status: StatusCode, code: &str) -> Response {
    json_response(status, &json!({"ok": false, "code": code}))
}

fn browser_endpoint_http_json(endpoint: crate::state_store::BrowserEndpointRecord) -> Value {
    json!({
        "endpoint_ref": endpoint.endpoint_ref,
        "device_id": endpoint.device_id,
        "browser_family": endpoint.browser_family,
        "extension_version": endpoint.extension_version,
        "consent": {
            "webchat_control": endpoint.webchat_control_allowed,
            "tool_bridge": endpoint.tool_bridge_allowed,
            "tool_bridge_workstation_mutation": endpoint.tool_bridge_mutation_allowed,
            "revision": endpoint.consent_revision,
        },
        "consent_revision": endpoint.consent_revision,
        "first_observed_at": endpoint.first_observed_at,
        "last_observed_at": endpoint.last_observed_at,
    })
}

fn browser_resource_http_json(resource: crate::state_store::BrowserResourceRecord) -> Value {
    json!({
        "resource_ref": resource.resource_ref,
        "endpoint_ref": resource.endpoint_ref,
        "provider": resource.provider,
        "kind": resource.kind,
        "parent_ref": resource.parent_ref,
        "display_label": resource.display_label,
        "observation_generation": resource.observation_generation,
        "first_observed_at": resource.first_observed_at,
        "last_observed_at": resource.last_observed_at,
    })
}

async fn get_extension_fleet(State(state): State<AppState>) -> Response {
    if !state.trusted_extension_ipc {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = tokio::task::spawn_blocking(|| {
        let paths = RuntimePaths::discover()?;
        crate::worker::extension_fleet_snapshot(&paths)
    })
    .await;
    match result {
        Ok(Ok(payload)) => json_response(StatusCode::OK, &payload),
        Ok(Err(error)) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "code": "device_inventory_unavailable", "error": error}),
        ),
        Err(error) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "code": "device_inventory_unavailable", "error": error.to_string()}),
        ),
    }
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
    let requested_continuity_id = payload
        .get("continuity_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let conversation_id = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if requested_continuity_id.is_none() && conversation_id.is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_id_or_conversation_id_required"}),
        );
    }
    if requested_continuity_id.is_some_and(|value| value.len() > 160)
        || conversation_id.is_some_and(|value| value.len() > 256)
    {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "continuity_field_too_large"}),
        );
    }
    let resolved = match state.state_store.lock() {
        Ok(store) => {
            let continuity_id = if let Some(continuity_id) = requested_continuity_id {
                Some(continuity_id.to_owned())
            } else if let Some(conversation_id) = conversation_id {
                match store.continuity_for_conversation(conversation_id) {
                    Ok(value) => value,
                    Err(error) if error == "continuity_binding_ambiguous" => {
                        return json_response(
                            StatusCode::CONFLICT,
                            &json!({"ok": false, "error": "continuity_ambiguous"}),
                        );
                    }
                    Err(error) => {
                        return json_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &json!({"ok": false, "error": error}),
                        );
                    }
                }
            } else {
                None
            };
            match continuity_id {
                Some(continuity_id) => store.continuity_resume(&continuity_id, 1),
                None => Ok(None),
            }
        }
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
    browser_actuation: BrowserActuationBroker,
    trusted_extension_ipc: bool,
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
    push_events_response(
        state.cache,
        filters,
        state.browser_actuation,
        state.trusted_extension_ipc,
    )
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

fn push_events_response(
    cache: Arc<EventCache>,
    filters: PushFilters,
    browser_actuation: BrowserActuationBroker,
    trusted_extension_ipc: bool,
) -> Response {
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
        browser_actuation,
        trusted_extension_ipc,
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

            if state.trusted_extension_ipc
                && let Some(command) = state.browser_actuation.take_next_for_extension()
            {
                body.push_str(&sse_event("browser_actuation", &command));
            }

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
    if trusted_edge_runtime_generation_fence(&state, &headers).is_err() {
        let mut response = json_response(
            StatusCode::CONFLICT,
            &json!({
                "jsonrpc": "2.0",
                "error": {"code": -32002, "message": "Runtime generation mismatch"},
                "id": null
            }),
        );
        attach_runtime_generation_header(&mut response);
        return response;
    }
    let caller_webchat_control_grants = match trusted_edge_webchat_control_grants(&state, &headers)
    {
        Ok(grants) => grants,
        Err(()) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": "Invalid trusted caller grant context"},
                    "id": null
                }),
            );
        }
    };
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
            // The workstation bearer authenticates only TCP transport. Browser business
            // authority is admitted exclusively from the trusted Unix IPC handoff above.
            caller_webchat_control_grants: &caller_webchat_control_grants,
            browser_actuator: Some(&blocking_state.browser_actuation),
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

fn trusted_edge_webchat_control_grants(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Vec<BrowserCallerGrant>, ()> {
    // The reserved handoff header is ignored on TCP even if a bearer-authenticated
    // client attempts to spoof it. Only the 0600 Unix IPC listener can elevate it.
    if !state.trusted_extension_ipc {
        return Ok(Vec::new());
    }
    let Some(raw) = headers.get(EDGE_WEBCHAT_CONTROL_GRANTS_HEADER) else {
        return Ok(Vec::new());
    };
    let text = raw.to_str().map_err(|_| ())?;
    if text.len() > MAX_EDGE_WEBCHAT_CONTROL_GRANTS_HEADER_BYTES {
        return Err(());
    }
    let value = serde_json::from_str::<Value>(text).map_err(|_| ())?;
    let items = value.as_array().ok_or(())?;
    if items.len() > 32 {
        return Err(());
    }
    let mut grants = Vec::with_capacity(items.len());
    for item in items {
        let object = item.as_object().ok_or(())?;
        if object.len() != 3
            || !object.contains_key("endpoint_ref")
            || !object.contains_key("provider")
            || !object.contains_key("account_ref")
        {
            return Err(());
        }
        let endpoint_ref = object
            .get("endpoint_ref")
            .and_then(Value::as_str)
            .ok_or(())?;
        let provider = object.get("provider").and_then(Value::as_str).ok_or(())?;
        let account_ref = object
            .get("account_ref")
            .and_then(Value::as_str)
            .ok_or(())?;
        if !valid_browser_grant_ref(endpoint_ref, 96)
            || !valid_browser_grant_provider(provider)
            || !valid_browser_grant_ref(account_ref, 96)
        {
            return Err(());
        }
        grants.push(BrowserCallerGrant {
            endpoint_ref: endpoint_ref.to_owned(),
            provider: provider.to_owned(),
            account_ref: account_ref.to_owned(),
        });
    }
    Ok(grants)
}

fn trusted_edge_runtime_generation_fence(state: &AppState, headers: &HeaderMap) -> Result<(), ()> {
    let current = env::var("HERDR_MCP_RUNTIME_GENERATION").ok();
    trusted_edge_runtime_generation_fence_with_current(state, headers, current.as_deref())
}

fn trusted_edge_runtime_generation_fence_with_current(
    state: &AppState,
    headers: &HeaderMap,
    current_generation: Option<&str>,
) -> Result<(), ()> {
    if !state.trusted_extension_ipc || !headers.contains_key(EDGE_WEBCHAT_CONTROL_GRANTS_HEADER) {
        return Ok(());
    }
    let expected = headers
        .get(EDGE_EXPECTED_RUNTIME_GENERATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    match current_generation.filter(|value| !value.is_empty()) {
        Some(current) if current == expected => Ok(()),
        _ => Err(()),
    }
}

fn valid_browser_grant_ref(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_browser_grant_provider(value: &str) -> bool {
    if value.is_empty() || value.len() > 32 {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
        })
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
            local_device_id: Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            browser_actuation: BrowserActuationBroker::default(),
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
        let response = tcp.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let fleet = Request::builder()
            .method(Method::GET)
            .uri("/extension/fleet")
            .header(AUTHORIZATION, "Bearer test-token")
            .body(Body::empty())
            .unwrap();
        let response = tcp.oneshot(fleet).await.unwrap();
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
        let response = app.clone().oneshot(resolve).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["continuity_id"], "hc:test");

        let resolve_by_conversation = Request::builder()
            .method(Method::POST)
            .uri("/extension/continuity/resolve")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"conversation_id": "conv-1"}).to_string()))
            .unwrap();
        let response = app.oneshot(resolve_by_conversation).await.unwrap();
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

    #[tokio::test]
    async fn browser_registry_route_is_trusted_local_and_device_authoritative() {
        let root = test_root("browser-registry-route");
        let register = json!({
            "operation": "endpoint.register",
            "profile_seed": "extension-profile-seed-0123456789abcdef",
            "browser_family": "chrome",
            "extension_version": "0.1.90",
            "observed_at": 1000
        });
        let request = |payload: Value| {
            Request::builder()
                .method(Method::POST)
                .uri("/extension/browser/registry")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap()
        };

        let tcp = candidate_router(test_state(&root.join("tcp")));
        let response = tcp.oneshot(request(register.clone())).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let mut extension_state = test_state(&root.join("extension"));
        extension_state.trusted_extension_ipc = true;
        extension_state.local_device_id = Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned());
        let store = extension_state.state_store.clone();
        let app = candidate_router(extension_state);

        let response = app
            .clone()
            .oneshot(request(register.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            result["endpoint"]["device_id"],
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        assert_eq!(result["endpoint"]["consent"]["webchat_control"], false);
        assert!(!result.to_string().contains("extension-profile-seed"));
        let endpoint_ref = result["endpoint"]["endpoint_ref"]
            .as_str()
            .unwrap()
            .to_owned();

        let injected = json!({
            "operation": "endpoint.register",
            "profile_seed": "extension-profile-seed-0123456789abcdef",
            "browser_family": "chrome",
            "extension_version": "0.1.90",
            "device_id": "dev_01M1E4VF6VGXAMGD0CN9WE8N7M"
        });
        let response = app.clone().oneshot(request(injected)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["code"], "browser_registry_params_invalid");

        let consent = json!({
            "operation": "endpoint.consent",
            "profile_seed": "extension-profile-seed-0123456789abcdef",
            "endpoint_ref": endpoint_ref,
            "expected_consent_revision": 0,
            "webchat_control_allowed": true,
            "tool_bridge_allowed": true,
            "tool_bridge_mutation_allowed": false,
            "observed_at": 1001
        });
        let response = app.clone().oneshot(request(consent)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let consent_res: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(consent_res["endpoint"]["consent"]["revision"], 1);
        assert_eq!(consent_res["endpoint"]["consent_revision"], 1);

        let provider = json!({
            "operation": "provider.observe",
            "profile_seed": "extension-profile-seed-0123456789abcdef",
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "adapter_protocol_version": 1,
            "observation_generation": 1,
            "capabilities": {"operations": ["identity.inspect", "space.list"]},
            "observed_at": 1002
        });
        let response = app.clone().oneshot(request(provider)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let account = json!({
            "operation": "resource.observe",
            "profile_seed": "extension-profile-seed-0123456789abcdef",
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "kind": "account",
            "native_identity": "native-account-do-not-return",
            "display_label": "Work",
            "observation_generation": 1,
            "observed_at": 1003
        });
        let response = app.clone().oneshot(request(account)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["ok"], true);
        assert!(!result.to_string().contains("native-account-do-not-return"));
        assert!(result["resource"].get("native_identity_sha256").is_none());

        let endpoint = store
            .lock()
            .unwrap()
            .browser_endpoint(&endpoint_ref)
            .unwrap()
            .unwrap();
        assert_eq!(endpoint.device_id, "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert!(endpoint.webchat_control_allowed);
        assert!(endpoint.tool_bridge_allowed);
        assert!(!endpoint.tool_bridge_mutation_allowed);
        assert_eq!(endpoint.consent_revision, 1);

        // Two-profile cross-profile mutation regression: Profile B cannot mutate Profile A
        let profile_b_seed = "extension-profile-seed-ffffffffffffffff";
        let reg_b = json!({
            "operation": "endpoint.register",
            "profile_seed": profile_b_seed,
            "browser_family": "chrome",
            "extension_version": "0.1.90",
            "observed_at": 1100
        });
        let response = app.clone().oneshot(request(reg_b)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let b_res: Value = serde_json::from_slice(&body).unwrap();
        let endpoint_ref_b = b_res["endpoint"]["endpoint_ref"].as_str().unwrap();
        assert_ne!(endpoint_ref_b, endpoint_ref);

        // Profile B attempts to mutate Profile A's consent (passes B seed but A endpoint_ref) -> fails
        let b_cross_consent = json!({
            "operation": "endpoint.consent",
            "profile_seed": profile_b_seed,
            "endpoint_ref": endpoint_ref,
            "expected_consent_revision": 1,
            "webchat_control_allowed": false,
            "tool_bridge_allowed": false,
            "tool_bridge_mutation_allowed": false,
            "observed_at": 1101
        });
        let response = app.clone().oneshot(request(b_cross_consent)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let cross_err: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(cross_err["code"], "browser_endpoint_ref_mismatch");

        // Profile B attempts to observe provider on Profile A -> fails
        let b_cross_provider = json!({
            "operation": "provider.observe",
            "profile_seed": profile_b_seed,
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "adapter_protocol_version": 1,
            "observation_generation": 2,
            "capabilities": {"operations": ["identity.inspect"]},
            "observed_at": 1102
        });
        let response = app
            .clone()
            .oneshot(request(b_cross_provider))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let cross_err: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(cross_err["code"], "browser_endpoint_ref_mismatch");

        // Profile B attempts to observe resource on Profile A -> fails
        let b_cross_resource = json!({
            "operation": "resource.observe",
            "profile_seed": profile_b_seed,
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "kind": "account",
            "native_identity": "attacker-account",
            "display_label": "Attacker",
            "observation_generation": 1,
            "observed_at": 1103
        });
        let response = app
            .clone()
            .oneshot(request(b_cross_resource))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let cross_err: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(cross_err["code"], "browser_endpoint_ref_mismatch");

        // Verify Profile A endpoint remains completely intact
        let endpoint_a = store
            .lock()
            .unwrap()
            .browser_endpoint(&endpoint_ref)
            .unwrap()
            .unwrap();
        assert!(endpoint_a.webchat_control_allowed);
        assert_eq!(endpoint_a.consent_revision, 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn browser_actuation_result_is_trusted_ipc_only_and_correlated() {
        let root = test_root("browser-actuation-route");
        let request = |payload: Value| {
            Request::builder()
                .method(Method::POST)
                .uri("/extension/browser/actuation")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap()
        };
        let evidence = |actuation_id: &str| {
            json!({
                "actuation_id": actuation_id,
                "observed_generation": 7,
                "command_accepted": true,
                "browser_online": true,
                "resource_available": true,
                "rejected": false,
                "stable_resource_ref_observed": true,
                "lifecycle_observed": true,
                "canonical_url_observed": true,
                "accepted_message_observed": true,
                "message_baseline_advanced": false,
                "reasoning_effort_readback": null,
                "required_apps_readback": [],
                "generation_owner": 7,
                "generation_status_observed": true,
                "generation_stopped": false
            })
        };

        let tcp = candidate_router(test_state(&root.join("tcp")));
        let response = tcp
            .oneshot(request(evidence("ba_0000000000000001")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let mut extension_state = test_state(&root.join("extension"));
        extension_state.trusted_extension_ipc = true;
        let broker = extension_state.browser_actuation.clone();
        assert!(broker.take_next_for_extension().is_none());
        let broker_for_task = broker.clone();
        let task = tokio::task::spawn_blocking(move || {
            broker_for_task.actuate(
                "herdr_mcp.browser_dispatch.submit",
                &json!({"session_ref":"br_test","message":"hello"}),
                7,
            )
        });
        let command = loop {
            if let Some(command) = broker.take_next_for_extension() {
                break command;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let actuation_id = command["actuation_id"].as_str().unwrap().to_owned();
        let app = candidate_router(extension_state);
        let response = app.oneshot(request(evidence(&actuation_id))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let observed = task.await.unwrap().unwrap();
        assert!(observed.command_accepted);
        assert!(observed.accepted_message_observed);
        assert_eq!(observed.generation_owner, Some(7));

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

    #[test]
    fn trusted_webchat_grant_generation_fence_is_predispatch_and_tcp_cannot_opt_in() {
        let root = test_root("browser-grant-generation-fence");
        let tcp_state = test_state(&root.join("tcp"));
        let mut headers = HeaderMap::new();
        headers.insert(
            EDGE_WEBCHAT_CONTROL_GRANTS_HEADER,
            HeaderValue::from_static(
                r#"[{"endpoint_ref":"be_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","provider":"chatgpt","account_ref":"br_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]"#,
            ),
        );
        headers.insert(
            EDGE_EXPECTED_RUNTIME_GENERATION_HEADER,
            HeaderValue::from_static("rust-old"),
        );
        assert_eq!(
            trusted_edge_runtime_generation_fence_with_current(
                &tcp_state,
                &headers,
                Some("rust-new")
            ),
            Ok(()),
            "ordinary TCP cannot activate the trusted generation-fence path"
        );

        let mut trusted_state = test_state(&root.join("trusted"));
        trusted_state.trusted_extension_ipc = true;
        assert_eq!(
            trusted_edge_runtime_generation_fence_with_current(
                &trusted_state,
                &headers,
                Some("rust-new")
            ),
            Err(())
        );
        assert_eq!(
            trusted_edge_runtime_generation_fence_with_current(
                &trusted_state,
                &headers,
                Some("rust-old")
            ),
            Ok(())
        );
        headers.remove(EDGE_EXPECTED_RUNTIME_GENERATION_HEADER);
        assert_eq!(
            trusted_edge_runtime_generation_fence_with_current(
                &trusted_state,
                &headers,
                Some("rust-old")
            ),
            Err(()),
            "grant-bearing trusted IPC without a reserved generation fails closed"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn workstation_bearer_does_not_become_webchat_control_grant() {
        let _env_guard = crate::test_env::lock();
        let previous_generation = env::var_os("HERDR_MCP_RUNTIME_GENERATION");
        unsafe { env::set_var("HERDR_MCP_RUNTIME_GENERATION", "rust-caller-grant-proof") };
        let root = test_root("browser-caller-grant-boundary");
        let state = test_state(&root);
        let (endpoint_ref, account_ref, session_ref) = {
            let mut store = state.state_store.lock().unwrap();
            let endpoint = store
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "caller-grant-boundary-profile",
                    browser_family: "chrome",
                    extension_version: "0.1.90",
                    observed_at: 10,
                })
                .unwrap();
            store
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["identity.inspect","session.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = store
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "native-account-hidden",
                    display_label: Some("Work"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            let session = store
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "native-session-hidden",
                    display_label: Some("Conversation"),
                    observation_generation: 7,
                    observed_at: 13,
                })
                .unwrap();
            store
                .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    expected_revision: 0,
                    webchat_control_allowed: true,
                    tool_bridge_allowed: false,
                    tool_bridge_mutation_allowed: false,
                    observed_at: 14,
                })
                .unwrap();
            (
                endpoint.endpoint_ref,
                account.resource_ref,
                session.resource_ref,
            )
        };

        let grant_header = serde_json::to_string(&json!([{
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "account_ref": account_ref,
        }]))
        .unwrap();
        let body = json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"tools/call",
            "params":{
                "name":"herdr_call",
                "arguments":{
                    "method":"herdr_mcp.browser_session.inspect",
                    "params": serde_json::to_string(&json!({"session_ref": session_ref})).unwrap()
                }
            }
        });

        let mut extension_state = state.clone();
        extension_state.trusted_extension_ipc = true;
        let tcp = candidate_router(state);
        let spoofed_tcp = rpc_request(
            Method::POST,
            "/mcp",
            Some(body.clone()),
            &[(EDGE_WEBCHAT_CONTROL_GRANTS_HEADER, grant_header.as_str())],
        );
        let response = tcp.oneshot(spoofed_tcp).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let text = payload["result"]["content"][0]["text"].as_str().unwrap();
        let local: Value = serde_json::from_str(text).unwrap();
        assert_eq!(local["ok"], true);
        assert_eq!(local["actuation_available"], false);
        assert_eq!(local["actuation_reason"], "caller_grant_missing");
        assert!(!local.to_string().contains("native-session-hidden"));

        let trusted = candidate_router(extension_state);
        let trusted_request = rpc_request(
            Method::POST,
            "/mcp",
            Some(body),
            &[
                (EDGE_WEBCHAT_CONTROL_GRANTS_HEADER, grant_header.as_str()),
                (
                    EDGE_EXPECTED_RUNTIME_GENERATION_HEADER,
                    "rust-caller-grant-proof",
                ),
            ],
        );
        let response = trusted.oneshot(trusted_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let text = payload["result"]["content"][0]["text"].as_str().unwrap();
        let local: Value = serde_json::from_str(text).unwrap();
        assert_eq!(local["ok"], true);
        assert_eq!(local["actuation_available"], true);
        assert!(local["actuation_reason"].is_null());
        assert!(!local.to_string().contains("native-session-hidden"));
        unsafe {
            match previous_generation {
                Some(value) => env::set_var("HERDR_MCP_RUNTIME_GENERATION", value),
                None => env::remove_var("HERDR_MCP_RUNTIME_GENERATION"),
            }
        }
        std::fs::remove_dir_all(root).ok();
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
            browser_actuation: BrowserActuationBroker::default(),
            trusted_extension_ipc: false,
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
