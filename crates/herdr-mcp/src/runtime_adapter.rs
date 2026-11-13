//! Runtime backend abstraction and narrow fail-closed `herdr-core` (`hc`) adapter (#768, #919).
//!
//! Design invariants (from #767 spike `docs/_wip/herdr-core-probe.md`):
//! 1. Explicit per-device selection (`tui` default vs `herdr-core`) with **zero silent fallback**
//!    between runtimes because they share neither agents nor terminals.
//! 2. Frozen Runtime Execution Contract Epoch 5 (18 public tools) stays fixed; tools or methods
//!    outside the active backend's dynamically negotiated capabilities fail closed with
//!    `runtime_capability_unsupported` (`retryable: false`, `delivery_state: "not_delivered"`).
//! 3. `HerdrCoreClient` negotiates `hello { generation: 1, ... }` on every connection and derives
//!    capabilities dynamically from `HelloResult.methods` and `HelloResult.features`.
//! 4. Human boundaries (`blocked` terminals) are never bypassed (`force` is hard-locked `false`),
//!    and remote binary/daemon mutation surfaces (`machine.add`, `server.stop`, etc.) are refused.

use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::paths::RuntimePaths;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const HERDR_CORE_PROTOCOL_GENERATION: u32 = 1;
pub const RUNTIME_BACKEND_ENV: &str = "HERDR_MCP_RUNTIME_BACKEND";

const HC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HC_EXEC_MAX_TIMEOUT: Duration = Duration::from_secs(60);
// `file.read` returns up to 8 MiB of text in one newline-delimited reply, and
// JSON string escaping can roughly double that (`"` and `\\` become two bytes),
// so the reply cap must clear the escaped bound with room for the envelope.
const HC_MAX_LINE_BYTES: usize = 20 * 1024 * 1024;
/// Chunk size for reading one reply; a reply is never read byte-at-a-time.
const HC_READ_CHUNK_BYTES: usize = 8 * 1024;
/// Symlink hops followed while resolving a path for the secret-path guard.
const HC_SECRET_PATH_SYMLINK_HOPS: usize = 16;

/// Raw-byte bound `hc 0.1.20` enforces on `file.write` ("base64, at most 8 MB raw").
pub const HC_FILE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// `terminal.wait` is a bounded check-in for an automated caller; a wait may
/// never hold a tool call open for the `hc` default of 180 s.
pub const HC_TERMINAL_WAIT_MAX_SECS: f64 = 20.0;

/// Credential directories that `fs_security::denied_secret_path` does not model
/// but that herdr-core can reach with no Herdr snapshot in front of it.
const HC_EXTRA_SECRET_SEGMENTS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".htpasswd",
    ".password-store",
];
static NEXT_HC_RPC_ID: AtomicU64 = AtomicU64::new(1);

const PROHIBITED_HC_METHODS: &[&str] = &[
    "machine.add",
    "machine.connect",
    "machine.rename",
    "machine.rm",
    "server.stop",
    "server.restart",
];

/// Raw forwarding is limited to metadata methods that do not expose files or
/// terminals outside the public tools' access policies. File, document, machine,
/// and terminal methods fail closed until equivalent safeguards are available.
const HERDR_CALL_FORWARDABLE_HC_METHODS: &[&str] =
    &["rpc.discover", "server.status", "templates.list"];

fn hc_forwardable_readonly_method(method: &str) -> bool {
    HERDR_CALL_FORWARDABLE_HC_METHODS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(method))
}

/// Why raw `herdr_call` forwarding must refuse `method`, or `None` when it
/// stays inside the read-only allowlist.
///
/// The caller passes the already-trimmed method name; whitespace is never part
/// of any decision (#936 P1-2).
fn hc_forwarding_refusal(method: &str) -> Option<String> {
    if method.is_empty() {
        return Some("herdr_call requires a non-empty method name".to_owned());
    }
    if PROHIBITED_HC_METHODS
        .iter()
        .any(|prohibited| prohibited.eq_ignore_ascii_case(method))
    {
        return Some(format!(
            "herdr-core method '{method}' is prohibited by herdr-mcp runtime ownership rules"
        ));
    }
    if method.to_ascii_lowercase().starts_with("machine.") {
        return Some(format!(
            "herdr-core method '{method}' is not forwardable through herdr_call; machine execution is unavailable on this backend"
        ));
    }
    if !hc_forwardable_readonly_method(method) {
        return Some(format!(
            "herdr-core method '{method}' is not forwardable through herdr_call: raw forwarding is limited to the read-only allowlist [{}]; file, document, terminal and machine operations are unavailable through this raw route",
            HERDR_CALL_FORWARDABLE_HC_METHODS.join(", "),
        ));
    }
    None
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RuntimeBackend {
    #[default]
    #[serde(rename = "tui")]
    Tui,
    #[serde(rename = "herdr-core")]
    HerdrCore,
}

impl RuntimeBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::HerdrCore => "herdr-core",
        }
    }

    pub const fn is_default_tui(&self) -> bool {
        matches!(self, Self::Tui)
    }

    pub fn parse_explicit(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "tui" => Ok(Self::Tui),
            "herdr-core" => Ok(Self::HerdrCore),
            other => Err(format!(
                "invalid runtime backend '{other}': expected 'tui' or 'herdr-core' (no fallback between runtimes)"
            )),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeBackendSelection {
    pub backend: RuntimeBackend,
    pub source: &'static str,
    pub herdr_core_socket: Option<PathBuf>,
}

impl RuntimeBackendSelection {
    pub fn summary_json(&self) -> Value {
        json!({
            "selected": self.backend.as_str(),
            "source": self.source,
            "fallback_allowed": false,
            "herdr_core_socket": self
                .herdr_core_socket
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        })
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct EnvOverrides {
    pub runtime_backend: Option<String>,
    pub hc_socket: Option<PathBuf>,
    pub hc_session: Option<std::ffi::OsString>,
    pub xdg_state_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl EnvOverrides {
    pub fn from_process() -> Result<Self, String> {
        Ok(Self {
            runtime_backend: optional_unicode_env(RUNTIME_BACKEND_ENV)?,
            hc_socket: std::env::var_os("HC_SOCKET").map(PathBuf::from),
            hc_session: std::env::var_os("HC_SESSION"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
        })
    }
}

fn optional_unicode_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must be valid UTF-8")),
    }
}

pub fn resolve_backend_selection_with_env(
    config: Option<&Config>,
    config_explicit: bool,
    env: &EnvOverrides,
) -> Result<RuntimeBackendSelection, String> {
    let (backend, source) = if let Some(raw) = env.runtime_backend.as_deref() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "{RUNTIME_BACKEND_ENV} is empty; set 'tui' or 'herdr-core', or unset it"
            ));
        }
        (RuntimeBackend::parse_explicit(trimmed)?, "env")
    } else if let Some(cfg) = config {
        let source = if config_explicit || cfg.runtime_backend != RuntimeBackend::Tui {
            "config"
        } else {
            "default"
        };
        (cfg.runtime_backend, source)
    } else {
        (RuntimeBackend::Tui, "default")
    };

    let herdr_core_socket = if backend == RuntimeBackend::HerdrCore {
        Some(resolve_herdr_core_socket_path(config, env)?)
    } else {
        config
            .and_then(|cfg| cfg.runtime_herdr_core_socket.as_deref())
            .map(PathBuf::from)
    };

    Ok(RuntimeBackendSelection {
        backend,
        source,
        herdr_core_socket,
    })
}

pub fn resolve_backend_selection(config: &Config) -> Result<RuntimeBackendSelection, String> {
    let explicit =
        config.runtime_backend != RuntimeBackend::Tui || config.runtime_herdr_core_socket.is_some();
    resolve_backend_selection_with_env(Some(config), explicit, &EnvOverrides::from_process()?)
}

pub fn resolve_process_backend_selection() -> Result<RuntimeBackendSelection, String> {
    let paths = RuntimePaths::discover()?;
    resolve_backend_selection_from_path(
        &paths.config_file,
        &paths.instance,
        &EnvOverrides::from_process()?,
    )
}

// Use the same instance and config formats as startup, but never migrate files
// during admission. An unreadable config or path-discovery failure must not
// silently select TUI. This read is not an atomic snapshot across later dispatch.
fn resolve_backend_selection_from_path(
    path: &Path,
    instance: &crate::instance::InstanceId,
    env: &EnvOverrides,
) -> Result<RuntimeBackendSelection, String> {
    let (cfg, _) = Config::read_only_for_instance(path, instance)?;
    let explicit =
        cfg.runtime_backend != RuntimeBackend::Tui || cfg.runtime_herdr_core_socket.is_some();
    resolve_backend_selection_with_env(Some(&cfg), explicit, env)
}

pub fn resolve_herdr_core_socket_path(
    config: Option<&Config>,
    env: &EnvOverrides,
) -> Result<PathBuf, String> {
    if let Some(configured) = config.and_then(|cfg| cfg.runtime_herdr_core_socket.as_deref()) {
        let trimmed = configured.trim();
        if trimmed.is_empty() {
            return Err("runtime.herdr_core_socket cannot be empty".to_owned());
        }
        return Ok(PathBuf::from(trimmed));
    }
    if let Some(socket) = &env.hc_socket {
        if socket.as_os_str().is_empty() {
            return Err("HC_SOCKET cannot be empty".to_owned());
        }
        return Ok(socket.clone());
    }
    let session = match env.hc_session.as_deref() {
        Some(raw) => {
            let raw = raw
                .to_str()
                .ok_or_else(|| "HC_SESSION must be valid UTF-8".to_owned())?;
            let trimmed = raw.trim();
            // `.`/`..` would escape the session directory while keeping the fixed
            // `<state>/herdr-core/<session>/<session>.sock` shape.
            if trimmed.is_empty()
                || trimmed == "."
                || trimmed == ".."
                || trimmed.len() > 64
                || !trimmed
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(format!(
                    "invalid HC_SESSION '{trimmed}': must be 1..=64 ASCII [A-Za-z0-9._-] characters and not '.' or '..'"
                ));
            }
            trimmed
        }
        None => "default",
    };
    let state_base = if let Some(xdg) = &env.xdg_state_home
        && !xdg.as_os_str().is_empty()
    {
        xdg.clone()
    } else if let Some(home) = &env.home
        && !home.as_os_str().is_empty()
    {
        home.join(".local/state")
    } else {
        return Err(
            "cannot resolve herdr-core socket path: set runtime.herdr_core_socket, HC_SOCKET, XDG_STATE_HOME, or HOME"
                .to_owned(),
        );
    };
    Ok(state_base
        .join("herdr-core")
        .join(session)
        .join(format!("{session}.sock")))
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeCapabilities {
    pub backend: RuntimeBackend,
    pub workspace_pane_graph: bool,
    pub agent_registry: bool,
    pub server_status: bool,
    pub terminal_lifecycle: bool,
    pub terminal_send: bool,
    pub machine_exec: bool,
    pub file_read: bool,
    pub file_write: bool,
    pub file_edit: bool,
    pub fs_list_grep_image_patch: bool,
    pub structured_git: bool,
    pub events_subscribe: bool,
    pub fenced_idempotent_prompt: bool,
}

impl RuntimeCapabilities {
    pub const fn full_tui() -> Self {
        Self {
            backend: RuntimeBackend::Tui,
            workspace_pane_graph: true,
            agent_registry: true,
            server_status: true,
            terminal_lifecycle: true,
            terminal_send: true,
            machine_exec: true,
            file_read: true,
            file_write: true,
            file_edit: true,
            fs_list_grep_image_patch: true,
            structured_git: true,
            events_subscribe: true,
            fenced_idempotent_prompt: true,
        }
    }

    pub fn from_herdr_core_methods(methods: &BTreeSet<String>) -> Self {
        let terminal_lifecycle = [
            "terminal.list",
            "terminal.get",
            "terminal.read",
            "terminal.run",
            "terminal.kill",
        ]
        .iter()
        .all(|method| methods.contains(*method));
        Self {
            backend: RuntimeBackend::HerdrCore,
            workspace_pane_graph: false,
            agent_registry: false,
            server_status: methods.contains("server.status"),
            terminal_lifecycle,
            terminal_send: methods.contains("terminal.send"),
            machine_exec: methods.contains("machine.exec"),
            file_read: methods.contains("file.read"),
            file_write: methods.contains("file.write"),
            file_edit: methods.contains("file.edit"),
            fs_list_grep_image_patch: false,
            structured_git: false,
            events_subscribe: methods.contains("events.subscribe"),
            fenced_idempotent_prompt: false,
        }
    }

    pub fn as_json(&self) -> Value {
        json!({
            "backend": self.backend.as_str(),
            "workspace_pane_graph": self.workspace_pane_graph,
            "agent_registry": self.agent_registry,
            "server_status": self.server_status,
            "terminal_lifecycle": self.terminal_lifecycle,
            "terminal_send": self.terminal_send,
            "machine_exec": self.machine_exec,
            "file_read": self.file_read,
            "file_write": self.file_write,
            "file_edit": self.file_edit,
            "fs_list_grep_image_patch": self.fs_list_grep_image_patch,
            "structured_git": self.structured_git,
            "events_subscribe": self.events_subscribe,
            "fenced_idempotent_prompt": self.fenced_idempotent_prompt,
        })
    }

    pub fn check_public_tool(&self, tool_name: &str) -> Result<(), Value> {
        let required = match tool_name {
            "herdr_skill" | "herdr_call" => return Ok(()),
            "herdr_methods" | "herdr_inspect" => ("server_status", self.server_status),
            "herdr_since" => ("events_subscribe", self.events_subscribe),
            "herdr_fs_read" => ("file_read", self.file_read),
            "herdr_fs_write" => ("file_write", self.file_write),
            "herdr_fs_edit" => ("file_edit", self.file_edit),
            "herdr_fs_list" | "herdr_fs_grep" | "herdr_fs_image" | "herdr_fs_patch" => {
                ("fs_list_grep_image_patch", self.fs_list_grep_image_patch)
            }
            "herdr_git" => ("structured_git", self.structured_git),
            "herdr_exec" => ("machine_exec", self.machine_exec),
            "herdr_exec_start" | "herdr_exec_read" | "herdr_exec_kill" => {
                ("terminal_lifecycle", self.terminal_lifecycle)
            }
            "herdr_prompt" => ("terminal_send", self.terminal_send),
            _ => return Ok(()),
        };
        if required.1 {
            Ok(())
        } else {
            Err(unsupported_capability_error(
                self.backend,
                tool_name,
                required.0,
            ))
        }
    }
}

pub fn unsupported_capability_error(
    backend: RuntimeBackend,
    operation: &str,
    required_capability: &str,
) -> Value {
    json!({
        "ok": false,
        "code": "runtime_capability_unsupported",
        "runtime_backend": backend.as_str(),
        "operation": operation,
        "required_capability": required_capability,
        "fallback_allowed": false,
        "retryable": false,
        "delivery_state": "not_delivered",
    })
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HerdrCoreError {
    pub code: String,
    pub kind: Option<String>,
    pub message: String,
}

impl std::fmt::Display for HerdrCoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(kind) = &self.kind {
            write!(formatter, "{} ({kind}): {}", self.code, self.message)
        } else {
            write!(formatter, "{}: {}", self.code, self.message)
        }
    }
}

impl std::error::Error for HerdrCoreError {}

#[allow(dead_code)]
pub trait HerdrRuntimeAdapter {
    fn backend(&self) -> RuntimeBackend;
    fn capabilities(&self) -> Result<RuntimeCapabilities, HerdrCoreError>;
    fn check_tool_supported(&self, tool_name: &str) -> Result<(), Value> {
        let caps = self.capabilities().map_err(|error| {
            json!({
                "ok": false,
                "code": error.code,
                "message": error.message,
                "runtime_backend": self.backend().as_str(),
                "fallback_allowed": false,
                "retryable": false,
                "delivery_state": "not_delivered",
            })
        })?;
        caps.check_public_tool(tool_name)
    }
    fn status_summary(&self) -> Value;
}

#[allow(dead_code)]
pub struct TuiRuntimeAdapter<'a> {
    client: &'a HerdrClient,
}

#[allow(dead_code)]
impl<'a> TuiRuntimeAdapter<'a> {
    pub const fn new(client: &'a HerdrClient) -> Self {
        Self { client }
    }

    pub const fn client(&self) -> &'a HerdrClient {
        self.client
    }
}

impl HerdrRuntimeAdapter for TuiRuntimeAdapter<'_> {
    fn backend(&self) -> RuntimeBackend {
        RuntimeBackend::Tui
    }

    fn capabilities(&self) -> Result<RuntimeCapabilities, HerdrCoreError> {
        Ok(RuntimeCapabilities::full_tui())
    }

    fn status_summary(&self) -> Value {
        match self.client.ping() {
            Ok(pong) => json!({
                "ok": true,
                "backend": "tui",
                "version": pong.get("version").cloned().unwrap_or(Value::Null),
                "protocol": pong.get("protocol").cloned().unwrap_or(Value::Null),
                "capabilities": RuntimeCapabilities::full_tui().as_json(),
            }),
            Err(error) => json!({
                "ok": false,
                "backend": "tui",
                "code": error.code,
                "message": error.message,
            }),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HerdrCoreHello {
    pub generation: u32,
    pub version: String,
    pub core_id: Option<String>,
    pub core_name: Option<String>,
    pub framed: bool,
    pub methods: BTreeSet<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HerdrCoreSessionReport {
    pub hello: HerdrCoreHello,
    pub capabilities: RuntimeCapabilities,
    pub live_terminals: u64,
    pub finished_terminals: u64,
}

/// Typed `terminal.wait` outcome (`hc 0.1.20` `WaitResult`).
///
/// `ended: false` is a bounded check-in, never a failure: the terminal is still
/// running and the caller decides whether to wait again.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct WaitResult {
    pub ended: bool,
    pub reason: String,
    pub status: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub turn: u64,
}

impl WaitResult {
    /// Parse one `terminal.wait` result. Missing optional facts stay `None`
    /// instead of being guessed from screen text (OSC 7501 rule, #767 §3.1).
    pub fn from_hc_result(raw: &Value) -> Self {
        Self {
            ended: raw.get("ended").and_then(Value::as_bool).unwrap_or(false),
            reason: raw
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unspecified")
                .to_owned(),
            status: raw.get("status").and_then(Value::as_str).map(str::to_owned),
            exit_code: raw
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            signal: raw
                .get("signal")
                .and_then(Value::as_i64)
                .and_then(|signal| i32::try_from(signal).ok()),
            turn: raw.get("turn").and_then(Value::as_u64).unwrap_or(0),
        }
    }

    #[allow(dead_code)]
    pub fn as_json(&self) -> Value {
        json!({
            "ended": self.ended,
            "reason": self.reason,
            "status": self.status,
            "exit_code": self.exit_code,
            "signal": self.signal,
            "turn": self.turn,
        })
    }
}

#[derive(Debug, Clone)]
pub struct HerdrCoreClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl HerdrCoreClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: HC_HANDSHAKE_TIMEOUT,
        }
    }

    #[allow(dead_code)]
    pub fn with_timeout(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: timeout.min(HC_EXEC_MAX_TIMEOUT),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn handshake(&self) -> Result<HerdrCoreHello, HerdrCoreError> {
        self.with_negotiated_session(|hello, _stream| Ok(hello))
    }

    pub fn probe_status(&self) -> Result<HerdrCoreSessionReport, HerdrCoreError> {
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            let capabilities = RuntimeCapabilities::from_herdr_core_methods(&hello.methods);
            if !capabilities.server_status {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: "herdr-core hello did not advertise server.status".to_owned(),
                });
            }
            let status = rpc_call_line(
                stream,
                &self.socket_path,
                "server.status",
                json!({}),
                call_timeout,
            )?;
            let live_terminals = status.get("live").and_then(Value::as_u64).unwrap_or(0);
            let finished_terminals = status.get("finished").and_then(Value::as_u64).unwrap_or(0);
            Ok(HerdrCoreSessionReport {
                hello,
                capabilities,
                live_terminals,
                finished_terminals,
            })
        })
    }

    pub fn inspect_terminals(&self) -> Result<Value, HerdrCoreError> {
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            let capabilities = RuntimeCapabilities::from_herdr_core_methods(&hello.methods);
            if !capabilities.server_status {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message:
                        "herdr-core did not advertise server.status in hello.methods"
                            .to_owned(),
                });
            }
            let status = rpc_call_line(stream, &self.socket_path, "server.status", json!({}), call_timeout)?;
            let mut methods_vec: Vec<String> = hello.methods.iter().cloned().collect();
            methods_vec.sort();
            Ok(json!({
                "ok": true,
                "herdr_core": {
                    "version": hello.version,
                    "protocol_generation": hello.generation,
                    "core_id": hello.core_id,
                    "core_name": hello.core_name,
                    "methods_count": methods_vec.len(),
                    "methods": methods_vec,
                    "features": hello.features,
                    "live_terminals": status.get("live").and_then(Value::as_u64).unwrap_or(0),
                    "finished_terminals": status.get("finished").and_then(Value::as_u64).unwrap_or(0),
                    "terminal_counts_scope": "global, not visibility-filtered",
                    "capabilities": capabilities.as_json(),
                    "capabilities_scope": "advertised herdr-core methods, not public tool availability",
                    "public_tools_policy_blocked": herdr_core_policy_blocked_tools(),
                    "public_tools_policy_scope": "policy blocks only; other tools may be unavailable and herdr_call is method-restricted",
                },
                "terminals": [],
                "terminal_details": "unavailable_without_ownership",
                "workspaces": [],
                "tabs": [],
                "panes": [],
                "agents": [],
            }))
        })
    }

    #[allow(dead_code)]
    pub fn machine_exec(
        &self,
        argv: &[String],
        cwd: Option<&Path>,
        timeout_secs: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        if argv.is_empty() {
            return Err(HerdrCoreError {
                code: "invalid_params".to_owned(),
                kind: None,
                message: "machine.exec argv must not be empty".to_owned(),
            });
        }
        let mut params = Map::new();
        params.insert("argv".to_owned(), json!(argv));
        self.machine_exec_params(params, cwd, timeout_secs)
    }

    /// `machine.exec` with a shell `line` instead of `argv`, so `herdr_exec`
    /// can forward a freeform `command` without re-splitting it locally.
    pub fn machine_exec_line(
        &self,
        line: &str,
        cwd: Option<&Path>,
        timeout_secs: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        if line.trim().is_empty() {
            return Err(HerdrCoreError {
                code: "invalid_params".to_owned(),
                kind: None,
                message: "machine.exec line must not be empty".to_owned(),
            });
        }
        let mut params = Map::new();
        params.insert("line".to_owned(), json!(line));
        self.machine_exec_params(params, cwd, timeout_secs)
    }

    fn machine_exec_params(
        &self,
        mut params: Map<String, Value>,
        cwd: Option<&Path>,
        timeout_secs: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        // P2-5: the global readonly gate lives in the shared primitive, so both
        // the `argv` (`machine_exec`) and shell (`machine_exec_line`) entrypoints
        // refuse before any socket work even when a future caller reaches them
        // without the typed `herdr_exec` wrapper.
        hc_mutation_guard("herdr_exec")?;
        // The read/write budget must match the server-side execution window, or a
        // long `machine.exec` would be cut off by the 5 s handshake timeout while
        // the command keeps running on the host.
        let effective_timeout_secs = timeout_secs.unwrap_or(60).clamp(1, 600);
        let call_timeout = Duration::from_secs(effective_timeout_secs) + HC_HANDSHAKE_TIMEOUT;
        self.with_negotiated_session_budget(call_timeout, |hello, stream| {
            if !hello.methods.contains("machine.exec") {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: "herdr-core did not advertise machine.exec".to_owned(),
                });
            }
            if let Some(cwd) = cwd {
                params.insert("cwd".to_owned(), json!(cwd.to_string_lossy().into_owned()));
            }
            params.insert("timeout".to_owned(), json!(effective_timeout_secs));
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "machine.exec",
                Value::Object(params),
                call_timeout,
            )?;
            let stdout = decode_hc_base64_field(&raw, "stdout")?;
            let stderr = decode_hc_base64_field(&raw, "stderr")?;
            let exit_code = raw.get("code").and_then(Value::as_i64);
            let timed_out = raw
                .get("timed_out")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let truncated = raw
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(json!({
                "ok": !timed_out && exit_code == Some(0),
                "runtime_backend": "herdr-core",
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "timed_out": timed_out,
                "truncated": truncated,
                "duration_ms": raw.get("ms").and_then(Value::as_u64),
                "signal": raw.get("signal").and_then(Value::as_i64),
            }))
        })
    }

    #[allow(dead_code)]
    pub fn terminal_send(
        &self,
        terminal: &str,
        text: &str,
        if_state: Option<&str>,
        wait: bool,
        timeout_secs: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        let terminal = terminal.trim();
        if terminal.is_empty() {
            return Err(HerdrCoreError {
                code: "invalid_params".to_owned(),
                kind: None,
                message: "terminal address is required".to_owned(),
            });
        }
        if let Some(if_state) = if_state
            && !matches!(if_state, "idle" | "working")
        {
            return Err(HerdrCoreError {
                code: "invalid_params".to_owned(),
                kind: None,
                message: "herdr-core terminal.send `if` only supports 'idle' or 'working'"
                    .to_owned(),
            });
        }
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            if !hello.methods.contains("terminal.send") {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: "herdr-core did not advertise terminal.send".to_owned(),
                });
            }
            let mut params = Map::new();
            params.insert("terminal".to_owned(), json!(terminal));
            params.insert("text".to_owned(), json!(text));
            // Human boundary invariant (#767 §3.3): `--force` is hard-locked false
            // so a `blocked` terminal waiting on human permission/question/auth is
            // never answered by an automated caller.
            params.insert("force".to_owned(), json!(false));
            params.insert("wait".to_owned(), json!(wait));
            if let Some(if_state) = if_state {
                params.insert("if".to_owned(), json!(if_state));
            }
            if let Some(timeout_secs) = timeout_secs {
                params.insert("timeout".to_owned(), json!(timeout_secs.clamp(1, 600)));
            }
            match rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.send",
                Value::Object(params),
                call_timeout,
            ) {
                Ok(result) => Ok(json!({
                    "ok": true,
                    "runtime_backend": "herdr-core",
                    "terminal": terminal,
                    "result": result,
                })),
                Err(err) if err.kind.as_deref() == Some("blocked") => Err(HerdrCoreError {
                    code: "human_boundary_blocked".to_owned(),
                    kind: Some("blocked".to_owned()),
                    message: err.message,
                }),
                Err(err) => Err(err),
            }
        })
    }

    /// `file.read`: the caller passes the *already resolved absolute* target, so
    /// `hc` cannot resolve it against a different process cwd. The same target is
    /// what the secret-path denylist checks, before the socket is contacted
    /// (fail-closed, no partial disclosure).
    #[allow(dead_code)]
    pub fn file_read(
        &self,
        target: &Path,
        offset_1based: Option<u64>,
        limit_lines: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        let path = hc_absolute_target(target, "file.read")?;
        hc_secret_like_path_guard(Path::new(&path))?;
        if offset_1based == Some(0) {
            return Err(hc_invalid_params(
                "file.read offset is 1-based; pass 1 or greater (omit for the start of the file)",
            ));
        }
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "file.read")?;
            let mut params = Map::new();
            params.insert("path".to_owned(), json!(path));
            if let Some(offset) = offset_1based {
                params.insert("offset".to_owned(), json!(offset));
            }
            if let Some(limit) = limit_lines {
                params.insert("limit".to_owned(), json!(limit));
            }
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "file.read",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "path": path,
                "text": raw.get("text").and_then(Value::as_str).unwrap_or(""),
                "first": raw.get("first").and_then(Value::as_u64).unwrap_or(0),
                "last": raw.get("last").and_then(Value::as_u64).unwrap_or(0),
                "lines": raw.get("lines").and_then(Value::as_u64).unwrap_or(0),
            }))
        })
    }

    /// `file.write`: readonly gate, secret-path refusal and the 8 MiB bound all
    /// run against the resolved absolute target before the socket is contacted;
    /// `data` is base64 per the OpenRPC schema.
    #[allow(dead_code)]
    pub fn file_write(&self, target: &Path, data: &[u8]) -> Result<Value, HerdrCoreError> {
        let path = hc_absolute_target(target, "file.write")?;
        hc_mutation_guard("herdr_fs_write")?;
        hc_secret_like_path_guard(Path::new(&path))?;
        if data.len() > HC_FILE_MAX_BYTES {
            return Err(HerdrCoreError {
                code: "file_too_large".to_owned(),
                kind: Some("file_too_large".to_owned()),
                message: format!(
                    "file.write payload is {} bytes; herdr-core accepts at most {HC_FILE_MAX_BYTES} bytes (8 MiB) per write",
                    data.len()
                ),
            });
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "file.write")?;
            let mut params = Map::new();
            params.insert("path".to_owned(), json!(path));
            params.insert("data".to_owned(), json!(encoded));
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "file.write",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "path": raw.get("path").and_then(Value::as_str).unwrap_or(&path),
                "bytes": raw.get("bytes").and_then(Value::as_u64).unwrap_or(data.len() as u64),
            }))
        })
    }

    /// `file.edit`: every `old` must match exactly once in the file as it was and
    /// `hc` applies all edits or none, so a failed call is never partially applied.
    /// The caller passes the resolved absolute target, which is also what the
    /// secret-path guard checks.
    #[allow(dead_code)]
    pub fn file_edit(
        &self,
        target: &Path,
        edits: &[(String, String)],
    ) -> Result<Value, HerdrCoreError> {
        let path = hc_absolute_target(target, "file.edit")?;
        if edits.is_empty() {
            return Err(hc_invalid_params(
                "file.edit requires at least one (old, new) replacement",
            ));
        }
        hc_mutation_guard("herdr_fs_edit")?;
        hc_secret_like_path_guard(Path::new(&path))?;
        // `edits` travels as one JSON array; bound the replacement text itself so a
        // call cannot allocate and send an arbitrarily large edit set.
        let total_edit_bytes = edits.iter().fold(0usize, |total, (old, new)| {
            total.saturating_add(old.len()).saturating_add(new.len())
        });
        if total_edit_bytes > HC_FILE_MAX_BYTES {
            return Err(HerdrCoreError {
                code: "edit_too_large".to_owned(),
                kind: Some("edit_too_large".to_owned()),
                message: format!(
                    "file.edit replacements total {total_edit_bytes} bytes; herdr-core edits are bounded to {HC_FILE_MAX_BYTES} bytes (8 MiB) of old+new text"
                ),
            });
        }
        let payload: Vec<Value> = edits
            .iter()
            .map(|(old, new)| json!({"old": old, "new": new}))
            .collect();
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "file.edit")?;
            let mut params = Map::new();
            params.insert("path".to_owned(), json!(path));
            params.insert("edits".to_owned(), Value::Array(payload));
            // `edit_failed`/`not_found`/`not_text` arrive as `herdr_core_<kind>`
            // with `kind` preserved, so callers never see a half-applied edit.
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "file.edit",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "path": raw.get("path").and_then(Value::as_str).unwrap_or(&path),
                "bytes": raw.get("bytes").and_then(Value::as_u64),
                "edits_applied": edits.len(),
            }))
        })
    }

    /// `terminal.run`: `argv` or `line` (never both), optional `cwd`/`name`/`labels`.
    ///
    /// No public herdr-mcp tool may call this yet (#936): `hc` terminals live in
    /// one address namespace shared with unrelated agent terminals and carry no
    /// runtime-scoped session id, so a started terminal could not be read or
    /// ended by a public route without inventing ownership. Kept as the typed
    /// wire mapping for a future owned-session design (`cwd` must already be the
    /// resolved absolute directory — see [`herdr_core_absolute_directory`]).
    #[allow(dead_code)]
    pub fn terminal_run(
        &self,
        argv: Option<&[String]>,
        line: Option<&str>,
        cwd: Option<&Path>,
        name: Option<&str>,
        labels: Option<&BTreeMap<String, String>>,
    ) -> Result<Value, HerdrCoreError> {
        let (argv, line) = match (argv, line) {
            (Some(argv), None) => {
                if argv.is_empty() {
                    return Err(hc_invalid_params(
                        "terminal.run argv must not be empty (omit it to send a line)",
                    ));
                }
                (Some(argv), None)
            }
            (None, Some(line)) => {
                if line.trim().is_empty() {
                    return Err(hc_invalid_params("terminal.run line must not be empty"));
                }
                (None, Some(line))
            }
            (Some(_), Some(_)) => {
                return Err(hc_invalid_params(
                    "terminal.run takes argv or line, never both",
                ));
            }
            (None, None) => {
                return Err(hc_invalid_params(
                    "terminal.run requires argv or line; an implicit shell is not started by a tool call",
                ));
            }
        };
        if let Some(name) = name {
            hc_validate_terminal_name(name)?;
        }
        hc_mutation_guard("herdr_exec_start")?;
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "terminal.run")?;
            let mut params = Map::new();
            if let Some(argv) = argv {
                params.insert("argv".to_owned(), json!(argv));
            }
            if let Some(line) = line {
                params.insert("line".to_owned(), json!(line));
            }
            hc_insert_cwd(&mut params, cwd);
            if let Some(name) = name {
                params.insert("name".to_owned(), json!(name));
            }
            if let Some(labels) = labels {
                params.insert("labels".to_owned(), json!(labels));
            }
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.run",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "terminal": raw.get("address").cloned().unwrap_or(Value::Null),
                "state": raw.get("state").cloned().unwrap_or(Value::Null),
                "cwd": raw.get("cwd").cloned().unwrap_or(Value::Null),
                "argv": raw.get("argv").cloned().unwrap_or_else(|| json!([])),
                "labels": raw.get("labels").cloned().unwrap_or_else(|| json!({})),
                "name": raw.get("name").cloned().unwrap_or(Value::Null),
                "parent": raw.get("parent").cloned().unwrap_or(Value::Null),
                "turn": raw.get("turn").cloned().unwrap_or(Value::Null),
                "created_ms": raw.get("created_ms").cloned().unwrap_or(Value::Null),
            }))
        })
    }

    /// `terminal.read`: screen plus optional history lines, live or finished.
    ///
    /// Not reachable from any public tool (#936): the address may belong to an
    /// unrelated agent terminal and there is no ownership or byte cursor, so
    /// `herdr_exec_read` and `herdr_call` refuse instead of disclosing a screen.
    #[allow(dead_code)]
    pub fn terminal_read(
        &self,
        terminal: &str,
        lines: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        let terminal = hc_required_terminal(terminal, "terminal.read")?;
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "terminal.read")?;
            let mut params = Map::new();
            params.insert("terminal".to_owned(), json!(terminal));
            if let Some(lines) = lines {
                params.insert("lines".to_owned(), json!(lines));
            }
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.read",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "terminal": terminal,
                "text": raw.get("text").and_then(Value::as_str).unwrap_or(""),
                // `complete: false` with a `why` (working/blocked/error) means a
                // full-screen program was not scrolled; never claim completeness.
                "complete": raw.get("complete").and_then(Value::as_bool).unwrap_or(false),
                "how": raw.get("how").and_then(Value::as_str).unwrap_or("screen"),
                "why": raw.get("why").cloned().unwrap_or(Value::Null),
            }))
        })
    }

    /// `terminal.kill`: ends the program and reports the observed terminal facts.
    ///
    /// Not reachable from any public tool (#936): the address may belong to an
    /// unrelated agent terminal and there is no ownership proof, so
    /// `herdr_exec_kill` and `herdr_call` refuse instead of killing that work.
    #[allow(dead_code)]
    pub fn terminal_kill(&self, terminal: &str) -> Result<Value, HerdrCoreError> {
        let terminal = hc_required_terminal(terminal, "terminal.kill")?;
        hc_mutation_guard("herdr_exec_kill")?;
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            hc_require_method(&hello, "terminal.kill")?;
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.kill",
                json!({"terminal": terminal}),
                call_timeout,
            )?;
            Ok(json!({
                "ok": true,
                "runtime_backend": "herdr-core",
                "terminal": raw.get("address").cloned().unwrap_or_else(|| json!(terminal)),
                "state": raw.get("state").cloned().unwrap_or(Value::Null),
                "reason": raw.get("reason").cloned().unwrap_or(Value::Null),
                "exit_code": raw.get("exit_code").cloned().unwrap_or(Value::Null),
                "signal": raw.get("signal").cloned().unwrap_or(Value::Null),
            }))
        })
    }

    /// `terminal.wait`: bounded check-in (`<= 20 s`), optionally ending on an
    /// `output` substring. `ended: false` means still running, not an error.
    #[allow(dead_code)]
    pub fn terminal_wait(
        &self,
        terminal: Option<&str>,
        output: Option<&str>,
        timeout_secs: Option<f64>,
    ) -> Result<WaitResult, HerdrCoreError> {
        let terminal = match terminal {
            Some(terminal) => Some(hc_required_terminal(terminal, "terminal.wait")?),
            None => None,
        };
        let output = match output {
            Some(output) => {
                if output.is_empty() {
                    return Err(hc_invalid_params(
                        "terminal.wait output match must not be empty (omit it to wait on status)",
                    ));
                }
                Some(output)
            }
            None => None,
        };
        let effective_timeout_secs = match timeout_secs {
            Some(seconds) => {
                if !seconds.is_finite() || seconds <= 0.0 {
                    return Err(hc_invalid_params(
                        "terminal.wait timeout must be a positive number of seconds",
                    ));
                }
                seconds.min(HC_TERMINAL_WAIT_MAX_SECS)
            }
            // Never let `hc` fall back to its own 180 s default: an omitted timeout
            // still means "bounded 20 s check-in" and is sent explicitly.
            None => HC_TERMINAL_WAIT_MAX_SECS,
        };
        // The connection budget must outlive the wait itself, otherwise the reply
        // would be cut off by the 5 s handshake timeout before `hc` answers.
        let call_timeout = Duration::from_secs_f64(effective_timeout_secs) + HC_HANDSHAKE_TIMEOUT;
        self.with_negotiated_session_budget(call_timeout, |hello, stream| {
            hc_require_method(&hello, "terminal.wait")?;
            let mut params = Map::new();
            if let Some(terminal) = terminal {
                params.insert("terminal".to_owned(), json!(terminal));
            }
            if let Some(output) = output {
                params.insert("output".to_owned(), json!(output));
            }
            params.insert("timeout".to_owned(), json!(effective_timeout_secs));
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.wait",
                Value::Object(params),
                call_timeout,
            )?;
            Ok(WaitResult::from_hc_result(&raw))
        })
    }

    #[allow(dead_code)]
    pub fn call_allowed_method(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, HerdrCoreError> {
        let method = method.trim();
        if let Some(message) = hc_forwarding_refusal(method) {
            // `terminal.send` keeps its dedicated human-boundary code: with
            // `force=true` the refusal is about the human boundary, not about
            // the read-only allowlist.
            if method.eq_ignore_ascii_case("terminal.send")
                && params.get("force").and_then(Value::as_bool) == Some(true)
            {
                return Err(HerdrCoreError {
                    code: "human_boundary_prohibited".to_owned(),
                    kind: None,
                    message: "terminal.send force=true is prohibited (human boundary)".to_owned(),
                });
            }
            return Err(HerdrCoreError {
                code: "herdr_core_method_prohibited".to_owned(),
                kind: None,
                message,
            });
        }
        let call_timeout = self.timeout;
        self.with_negotiated_session(|hello, stream| {
            if !hello.methods.contains(method) {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: format!("herdr-core hello.methods did not advertise '{method}'"),
                });
            }
            rpc_call_line(stream, &self.socket_path, method, params, call_timeout)
        })
    }

    fn with_negotiated_session<T>(
        &self,
        op: impl FnOnce(HerdrCoreHello, &mut HcSocketStream) -> Result<T, HerdrCoreError>,
    ) -> Result<T, HerdrCoreError> {
        self.with_negotiated_session_budget(self.timeout, op)
    }

    /// Negotiate `hello` and then run `op` with an explicit per-call budget.
    ///
    /// The budget is the *total* monotonic deadline for one JSON-RPC call, not a
    /// per-`read` idle timeout, so a slow peer cannot stretch a 5 s handshake
    /// window indefinitely by dripping bytes.
    fn with_negotiated_session_budget<T>(
        &self,
        call_timeout: Duration,
        op: impl FnOnce(HerdrCoreHello, &mut HcSocketStream) -> Result<T, HerdrCoreError>,
    ) -> Result<T, HerdrCoreError> {
        let mut stream = connect_hc_socket(&self.socket_path, call_timeout)?;
        let hello_raw = rpc_call_line(
            &mut stream,
            &self.socket_path,
            "hello",
            json!({
                "generation": HERDR_CORE_PROTOCOL_GENERATION,
                "calls_you": null,
                "framed": false,
                "core": {
                    "id": "herdr-mcp",
                    "name": "herdr-mcp"
                }
            }),
            call_timeout,
        )?;
        let hello = parse_hello_result(&hello_raw)?;
        op(hello, &mut stream)
    }
}

impl HerdrRuntimeAdapter for HerdrCoreClient {
    fn backend(&self) -> RuntimeBackend {
        RuntimeBackend::HerdrCore
    }

    fn capabilities(&self) -> Result<RuntimeCapabilities, HerdrCoreError> {
        let hello = self.handshake()?;
        Ok(RuntimeCapabilities::from_herdr_core_methods(&hello.methods))
    }

    fn status_summary(&self) -> Value {
        match self.probe_status() {
            Ok(report) => json!({
                "ok": true,
                "backend": "herdr-core",
                "socket_path": self.socket_path().to_string_lossy(),
                "version": report.hello.version,
                "protocol_generation": report.hello.generation,
                "methods_count": report.hello.methods.len(),
                "features": report.hello.features,
                "live_terminals": report.live_terminals,
                "finished_terminals": report.finished_terminals,
                "capabilities": report.capabilities.as_json(),
            }),
            Err(error) => json!({
                "ok": false,
                "backend": "herdr-core",
                "socket_path": self.socket_path().to_string_lossy(),
                "code": error.code,
                "kind": error.kind,
                "message": error.message,
            }),
        }
    }
}

fn hc_invalid_params(message: impl Into<String>) -> HerdrCoreError {
    HerdrCoreError {
        code: "invalid_params".to_owned(),
        kind: Some("invalid_params".to_owned()),
        message: message.into(),
    }
}

fn hc_require_method(hello: &HerdrCoreHello, method: &str) -> Result<(), HerdrCoreError> {
    if hello.methods.contains(method) {
        return Ok(());
    }
    Err(HerdrCoreError {
        code: "runtime_capability_unsupported".to_owned(),
        kind: None,
        message: format!("herdr-core hello.methods did not advertise '{method}'"),
    })
}

/// Global readonly gate (`HERDR_MCP_READONLY=1`) shared with every other mutating
/// surface, applied before any socket work so a refusal never reaches `hc`.
fn hc_mutation_guard(action: &str) -> Result<(), HerdrCoreError> {
    crate::mutation::check_global(action).map_err(|refusal| HerdrCoreError {
        code: "readonly_mode".to_owned(),
        kind: Some("readonly_mode".to_owned()),
        message: refusal
            .get("hint")
            .and_then(Value::as_str)
            .unwrap_or("all mutating operations are disabled")
            .to_owned(),
    })
}

/// Fail-closed secret-path refusal, reusing the crate-wide `fs_security`
/// denylist (`.env*`, `id_rsa*`, `*secret*`, `*.pem`, `*.key`, ...).
///
/// Check the supplied absolute target and its currently resolved path. A
/// harmless name can hide a secret behind a symlink or in a parent directory.
/// Filesystem state may change after this check and before hc opens the path;
/// path strings and repeated resolution do not prevent that race.
fn hc_secret_like_path_guard(target: &Path) -> Result<(), HerdrCoreError> {
    let resolved = hc_symlink_resolved_path(target, HC_SECRET_PATH_SYMLINK_HOPS)?;
    for checked in [target, resolved.as_path()] {
        if hc_secret_like_path(checked) {
            return Err(HerdrCoreError {
                code: "secret_path_denied".to_owned(),
                kind: Some("secret_path_denied".to_owned()),
                message: format!(
                    "refusing secret-like path '{}' (checked '{}'): herdr-core file primitives apply the fs_security denylist to the supplied and currently resolved paths",
                    target.display(),
                    checked.display()
                ),
            });
        }
    }
    Ok(())
}

/// Resolve one caller-supplied `herdr-core` directory (`root`/`project_root`/`cwd`)
/// to the absolute directory the operation will really use.
///
/// A relative value can never be honoured: `hc` resolves it against the
/// herdr-core process cwd while herdr-mcp resolves it against its own, so the
/// single-writer root lease, the secret-path denylist and the executed directory
/// would name different places (#936). Equivalence cannot be guaranteed, so the
/// call fails closed with `herdr_core_path_ambiguous`.
pub fn herdr_core_absolute_directory(raw: &str) -> Result<PathBuf, HerdrCoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(hc_invalid_params("directory argument must not be empty"));
    }
    if trimmed.contains('\0') {
        return Err(hc_invalid_params("directory argument must not contain NUL"));
    }
    let path = Path::new(trimmed);
    if path.is_relative() {
        return Err(hc_path_ambiguous(format!(
            "herdr-core cannot honour the relative directory '{trimmed}': hc resolves it against the herdr-core process cwd while herdr-mcp resolves it against its own, so the root lease and the secret-path guard would check a different directory than the one executed; pass an absolute path"
        )));
    }
    hc_resolved_absolute_path(path)
}

/// Resolve the one absolute file target a `herdr-core` `file.*` call will act on.
///
/// `cwd` must already be absolute ([`herdr_core_absolute_directory`]); a relative
/// `path` without an absolute `cwd` fails closed because `hc` would resolve it
/// against its own process cwd, which herdr-mcp cannot observe. The returned
/// path is resolved at this instant; separate lease checks and later opens can
/// observe different filesystem state. Public file routes remain blocked until
/// the authorization and open can be safely coupled.
pub fn herdr_core_absolute_file_target(
    path: &str,
    cwd: Option<&Path>,
) -> Result<PathBuf, HerdrCoreError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(hc_invalid_params("path is required"));
    }
    if trimmed.contains('\0') {
        return Err(hc_invalid_params("path must not contain NUL"));
    }
    let raw = Path::new(trimmed);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let Some(cwd) = cwd else {
            return Err(hc_path_ambiguous(format!(
                "herdr-core cannot honour the relative path '{trimmed}' without an absolute cwd: hc would resolve it against the herdr-core process cwd, which herdr-mcp cannot observe, so the target could differ from the file this call validated; pass an absolute path (or an absolute root/project_root/cwd)"
            )));
        };
        if cwd.is_relative() {
            return Err(hc_path_ambiguous(
                "herdr-core cannot resolve a relative path against a relative cwd; pass an absolute directory",
            ));
        }
        cwd.join(raw)
    };
    hc_resolved_absolute_path(&candidate)
}

/// Resolve each component in filesystem order, including symlinks before `..`.
fn hc_resolved_absolute_path(path: &Path) -> Result<PathBuf, HerdrCoreError> {
    hc_symlink_resolved_path(path, HC_SECRET_PATH_SYMLINK_HOPS)
}

fn hc_path_ambiguous(message: impl Into<String>) -> HerdrCoreError {
    HerdrCoreError {
        code: "herdr_core_path_ambiguous".to_owned(),
        kind: Some("herdr_core_path_ambiguous".to_owned()),
        message: message.into(),
    }
}

/// The exact UTF-8 path string put on the `hc` wire. A non-UTF-8 path cannot be
/// represented as a JSON string, so it fails closed instead of being lossily
/// rewritten into a different target.
fn hc_absolute_target(target: &Path, method: &str) -> Result<String, HerdrCoreError> {
    if !target.is_absolute() {
        return Err(hc_path_ambiguous(format!(
            "{method} requires an absolute resolved target; refusing to send a relative path to hc"
        )));
    }
    target.to_str().map(str::to_owned).ok_or_else(|| {
        hc_invalid_params(format!(
            "{method} target is not valid UTF-8 and cannot be sent to hc as a JSON path"
        ))
    })
}

/// `fs_security::denied_secret_path` plus credential directories that the
/// herdr-core file methods can reach without a Herdr snapshot in front of them.
fn hc_secret_like_path(path: &Path) -> bool {
    if crate::fs_security::denied_secret_path(path) {
        return true;
    }
    path.components().any(|component| {
        let segment = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        HC_EXTRA_SECRET_SEGMENTS.contains(&segment.as_str())
    })
}

/// Resolve components in filesystem order, including dangling symlinks. Missing
/// leaf components are retained, but errors other than NotFound fail closed.
fn hc_symlink_resolved_path(path: &Path, hops: usize) -> Result<PathBuf, HerdrCoreError> {
    use std::collections::VecDeque;
    use std::path::Component;

    if !path.is_absolute() {
        return Err(hc_path_ambiguous(
            "path resolution requires an absolute path",
        ));
    }
    let mut pending: VecDeque<_> = path
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::from("/");
    let mut followed = 0;
    let mut missing = false;
    while let Some(part) = pending.pop_front() {
        match Path::new(&part).components().next() {
            Some(Component::RootDir) => {
                resolved = PathBuf::from("/");
                missing = false;
            }
            Some(Component::CurDir) | None => {}
            Some(Component::ParentDir) => {
                if missing {
                    return Err(hc_path_ambiguous(format!(
                        "cannot resolve '..' through missing path '{}'",
                        resolved.display()
                    )));
                }
                resolved.pop();
            }
            Some(Component::Normal(_)) => {
                resolved.push(&part);
                match std::fs::symlink_metadata(&resolved) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        if followed >= hops {
                            return Err(hc_path_ambiguous(format!(
                                "symlink resolution limit exceeded at '{}'",
                                resolved.display()
                            )));
                        }
                        followed += 1;
                        let target = std::fs::read_link(&resolved).map_err(|error| {
                            hc_path_ambiguous(format!(
                                "cannot read symlink '{}': {error}",
                                resolved.display()
                            ))
                        })?;
                        resolved.pop();
                        let parts: Vec<_> = target
                            .components()
                            .map(|part| part.as_os_str().to_owned())
                            .collect();
                        for part in parts.into_iter().rev() {
                            pending.push_front(part);
                        }
                    }
                    Ok(metadata) => {
                        if !pending.is_empty() && !metadata.is_dir() {
                            return Err(hc_path_ambiguous(format!(
                                "path component '{}' is not a directory",
                                resolved.display()
                            )));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
                    Err(error) => {
                        return Err(hc_path_ambiguous(format!(
                            "cannot inspect path '{}': {error}",
                            resolved.display()
                        )));
                    }
                }
            }
            Some(Component::Prefix(_)) => {
                return Err(hc_path_ambiguous("unsupported path prefix"));
            }
        }
    }
    Ok(resolved)
}

fn hc_required_terminal<'a>(terminal: &'a str, method: &str) -> Result<&'a str, HerdrCoreError> {
    let terminal = terminal.trim();
    if terminal.is_empty() {
        return Err(hc_invalid_params(format!("{method} terminal is required")));
    }
    Ok(terminal)
}

fn hc_validate_terminal_name(name: &str) -> Result<(), HerdrCoreError> {
    let valid = !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(hc_invalid_params(format!(
            "invalid terminal name '{name}': herdr-core accepts 1..=32 of a-z 0-9 - _ ."
        )))
    }
}

fn hc_insert_cwd(params: &mut Map<String, Value>, cwd: Option<&Path>) {
    if let Some(cwd) = cwd {
        params.insert("cwd".to_owned(), json!(cwd.to_string_lossy().into_owned()));
    }
}

fn decode_hc_base64_field(raw: &Value, key: &str) -> Result<String, HerdrCoreError> {
    let Some(encoded) = raw.get(key).and_then(Value::as_str) else {
        return Ok(String::new());
    };
    if encoded.is_empty() {
        return Ok(String::new());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| HerdrCoreError {
            code: "parse_error".to_owned(),
            kind: None,
            message: format!("invalid base64 in herdr-core {key}: {error}"),
        })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_hello_result(raw: &Value) -> Result<HerdrCoreHello, HerdrCoreError> {
    let generation_u64 = raw
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| HerdrCoreError {
            code: "parse_error".to_owned(),
            kind: None,
            message: "herdr-core hello response missing integer generation".to_owned(),
        })?;
    let generation = u32::try_from(generation_u64).map_err(|_| HerdrCoreError {
        code: "unsupported_generation".to_owned(),
        kind: None,
        message: format!("herdr-core protocol generation {generation_u64} out of range"),
    })?;
    if generation != HERDR_CORE_PROTOCOL_GENERATION {
        return Err(HerdrCoreError {
            code: "unsupported_generation".to_owned(),
            kind: Some("unsupported_generation".to_owned()),
            message: format!(
                "herdr-core reported protocol generation {generation}, expected {HERDR_CORE_PROTOCOL_GENERATION}"
            ),
        });
    }
    let version = raw
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let framed = raw.get("framed").and_then(Value::as_bool).unwrap_or(false);
    // This adapter always negotiates `framed: false` and speaks newline-delimited
    // JSON-RPC, so a framed peer would desynchronize the connection: refuse it
    // instead of parsing framed bytes as lines.
    if framed {
        return Err(HerdrCoreError {
            code: "herdr_core_framed_unsupported".to_owned(),
            kind: Some("framed_unsupported".to_owned()),
            message: "herdr-core switched this connection to framed mode; herdr-mcp only speaks newline-delimited JSON-RPC".to_owned(),
        });
    }
    let methods = raw
        .get("methods")
        .and_then(Value::as_array)
        .ok_or_else(|| HerdrCoreError {
            code: "parse_error".to_owned(),
            kind: None,
            message: "herdr-core hello response missing methods[]".to_owned(),
        })?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let features = raw
        .get("features")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let core_id = raw
        .get("core")
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let core_name = raw
        .get("core")
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(HerdrCoreHello {
        generation,
        version,
        core_id,
        core_name,
        framed,
        methods,
        features,
    })
}

/// The backend route admitted for one already-declared public tool call.
///
/// `Tui` keeps the historical in-process snapshot/socket implementation;
/// `HerdrCore` carries the negotiated client so `mcp::tool_call` forwards the
/// narrow `hc` primitives instead of running the TUI path (#935).
pub enum PublicToolRoute {
    Tui,
    HerdrCore(HerdrCoreClient),
}

impl PublicToolRoute {
    pub fn herdr_core_client(&self) -> Option<&HerdrCoreClient> {
        match self {
            Self::Tui => None,
            Self::HerdrCore(client) => Some(client),
        }
    }
}

/// Fail-closed admission and routing for one frozen Epoch-5 public tool invocation (#922, #935).
///
/// The tool catalog itself never changes: this gate decides only whether the
/// *selected backend* may serve an already-declared tool. `tui` is the default
/// and admits everything without touching a socket; `herdr-core` re-negotiates
/// `hello` and applies the adapter's capability table. Any backend-selection
/// error (for example `HERDR_MCP_RUNTIME_BACKEND=auto`) fails closed instead of
/// falling back to the TUI path.
pub fn public_tool_route(tool_name: &str) -> Result<PublicToolRoute, Value> {
    let selection = resolve_process_backend_selection().map_err(|error| {
        invalid_runtime_backend_tool_error(tool_name, &error, "backend_selection")
    })?;
    public_tool_route_for_selection(tool_name, &selection)
}

fn public_tool_route_for_selection(
    tool_name: &str,
    selection: &RuntimeBackendSelection,
) -> Result<PublicToolRoute, Value> {
    match selection.backend {
        RuntimeBackend::Tui => Ok(PublicToolRoute::Tui),
        RuntimeBackend::HerdrCore => {
            // These routes lack the TUI's access and mutation policies and result contract.
            // Reject before even negotiating with hc, regardless of its capabilities.
            if CORE_POLICY_RESTRICTED_PUBLIC_TOOLS.contains(&tool_name) {
                return Err(herdr_core_unsupported(
                    tool_name,
                    "public_tool_policy_parity",
                    "the selected herdr-core backend cannot enforce the public tool's access and safety policies and result contract",
                ));
            }
            let Some(socket_path) = selection.herdr_core_socket.as_deref() else {
                return Err(json!({
                    "ok": false,
                    "code": "herdr_core_socket_unconfigured",
                    "operation": tool_name,
                    "runtime_backend": selection.summary_json(),
                    "fallback_allowed": false,
                    "retryable": false,
                    "delivery_state": "not_delivered",
                    "failure_phase": "backend_selection",
                    "message": "herdr-core backend selected without a resolved socket path",
                }));
            };
            let client = HerdrCoreClient::new(socket_path);
            let capability_check = client.check_tool_supported(tool_name).map_err(|mut rejection| {
                if let Some(object) = rejection.as_object_mut() {
                    object.insert("operation".to_owned(), json!(tool_name));
                    object.insert("failure_phase".to_owned(), json!("backend_capability"));
                    let message = match object.get("required_capability").and_then(Value::as_str) {
                        Some(capability) => format!(
                            "the selected herdr-core backend does not serve '{tool_name}' (required capability '{capability}'); no TUI fallback is allowed"
                        ),
                        None => format!(
                            "the selected herdr-core backend cannot serve '{tool_name}'; no TUI fallback is allowed"
                        ),
                    };
                    object.insert("message".to_owned(), json!(message));
                }
                rejection
            });
            capability_check?;
            // #936: an advertised `terminal.run` is necessary but never
            // sufficient for the `herdr_exec_*` session contract; see
            // `EXEC_SESSION_UNSUPPORTED_PUBLIC_TOOLS`.
            if EXEC_SESSION_UNSUPPORTED_PUBLIC_TOOLS.contains(&tool_name) {
                return Err(herdr_core_exec_session_unsupported(tool_name));
            }
            // A capability name in `hello` is necessary, never sufficient: the
            // in-process implementation of these tools still reads the live TUI
            // snapshot/socket, so admitting them would silently run on the TUI
            // runtime the operator did not select (#922).
            if TUI_ROUTED_PUBLIC_TOOLS.contains(&tool_name) {
                return Err(json!({
                    "ok": false,
                    "code": "runtime_capability_unsupported",
                    "operation": tool_name,
                    "required_capability": "public_tool_route",
                    "runtime_backend": selection.backend.as_str(),
                    "fallback_allowed": false,
                    "retryable": false,
                    "delivery_state": "not_delivered",
                    "failure_phase": "backend_capability",
                    "message": format!(
                        "'{tool_name}' is implemented on the Herdr TUI snapshot/socket and has no herdr-core route yet; it must not execute on the TUI backend when herdr-core is selected"
                    ),
                }));
            }
            Ok(PublicToolRoute::HerdrCore(client))
        }
    }
}

/// Public tools whose only implementation is the TUI snapshot/socket path.
///
/// These six have no `hc` primitive at all (no listing, search, image, patch,
/// git or event-cursor surface), so `herdr-core` keeps failing them closed with
/// `runtime_capability_unsupported` (#935).
const TUI_ROUTED_PUBLIC_TOOLS: &[&str] = &[
    "herdr_fs_list",
    "herdr_fs_grep",
    "herdr_fs_image",
    "herdr_fs_patch",
    "herdr_git",
    "herdr_since",
];

/// Public tools whose `herdr-core` semantics cannot be preserved safely (#936).
///
/// `hc terminal.run` creates a pty terminal (`t7`, a name, or `machine/t7`) in a
/// single address namespace that also holds every unrelated agent terminal, and
/// `hc` exposes no runtime-scoped session id, no ownership marker, no byte
/// cursor and no agent/repo binding. The public `herdr_exec_read` and
/// `herdr_exec_kill` contracts address a session by a caller-supplied
/// `session_id`, which is indistinguishable from another agent's terminal
/// address: forwarding it would disclose or kill work herdr-mcp does not own.
/// `herdr_exec_start` would create exactly such a session with no safe public
/// way to read or end it, i.e. an orphan claiming unsupported functionality.
///
/// All three fail closed instead of inventing an identity that `hc`
/// does not provide. One-shot `herdr_exec` is separately blocked until its
/// public access and mutation policy can be enforced.
const CORE_POLICY_RESTRICTED_PUBLIC_TOOLS: &[&str] = &[
    "herdr_fs_read",
    "herdr_fs_write",
    "herdr_fs_edit",
    "herdr_exec",
];
const EXEC_SESSION_UNSUPPORTED_PUBLIC_TOOLS: &[&str] =
    &["herdr_exec_start", "herdr_exec_read", "herdr_exec_kill"];

fn herdr_core_policy_blocked_tools() -> Vec<&'static str> {
    CORE_POLICY_RESTRICTED_PUBLIC_TOOLS
        .iter()
        .chain(EXEC_SESSION_UNSUPPORTED_PUBLIC_TOOLS.iter())
        .copied()
        .chain(std::iter::once("herdr_prompt"))
        .collect()
}

/// The single refusal envelope for the unowned `herdr_exec_*` session contract.
///
/// Shared by backend admission ([`public_tool_route`]) and the route-level
/// shims, so neither layer can be changed into creating or addressing an `hc`
/// terminal that herdr-mcp does not own.
pub fn herdr_core_exec_session_unsupported(operation: &str) -> Value {
    json!({
        "ok": false,
        "code": "runtime_capability_unsupported",
        "operation": operation,
        "required_capability": "exec_session_ownership",
        "runtime_backend": RuntimeBackend::HerdrCore.as_str(),
        "fallback_allowed": false,
        "retryable": false,
        "delivery_state": "not_delivered",
        "failure_phase": "backend_capability",
        "message": format!(
            "'{operation}' is refused on the herdr-core backend: hc terminal addresses are one namespace shared with every unrelated agent terminal, and hc exposes no runtime-scoped session id, ownership marker or byte cursor, so a caller-supplied session_id could read or kill work herdr-mcp does not own (and starting a session would create one no public route can safely read or end); one-shot execution also remains unavailable on this backend until public tool policy parity is provided"
        ),
    })
}

/// A public tool or forwarded method the selected `herdr-core` backend cannot
/// serve, reported with the same fail-closed envelope as every other admission
/// rejection.
pub fn herdr_core_unsupported(operation: &str, required_capability: &str, message: &str) -> Value {
    json!({
        "ok": false,
        "code": "runtime_capability_unsupported",
        "operation": operation,
        "required_capability": required_capability,
        "runtime_backend": RuntimeBackend::HerdrCore.as_str(),
        "fallback_allowed": false,
        "retryable": false,
        "delivery_state": "not_delivered",
        "failure_phase": "backend_capability",
        "message": message,
    })
}

fn invalid_runtime_backend_tool_error(tool_name: &str, error: &str, phase: &str) -> Value {
    json!({
        "ok": false,
        "code": "invalid_runtime_backend",
        "operation": tool_name,
        "message": error,
        "fallback_allowed": false,
        "retryable": false,
        "delivery_state": "not_delivered",
        "failure_phase": phase,
    })
}

// Narrow `herdr-core` tool routes (#935).
//
// Each route maps one already-admitted public tool onto a `HerdrCoreClient`
// primitive. They return the same `{ok: false, code, ...}` envelope as the TUI
// implementations so callers never have to branch on the backend, and every
// refusal carries `retryable: false` plus an explicit delivery state.

/// `herdr_methods` under `herdr-core`: the dynamically negotiated
/// `hello.methods` plus the herdr-mcp local method registry, filtered by
/// `query` (case-insensitive substring, matching the TUI inventory).
pub fn herdr_core_methods(client: &HerdrCoreClient, query: &str) -> Value {
    let hello = match client.handshake() {
        Ok(hello) => hello,
        Err(error) => {
            return herdr_core_error_json(&error, "herdr_methods", "not_delivered");
        }
    };
    let needle = query.trim().to_ascii_lowercase();
    let mut methods: Vec<Value> = hello
        .methods
        .iter()
        .filter(|method| needle.is_empty() || method.to_ascii_lowercase().contains(&needle))
        .map(|method| {
            json!({
                "method": method,
                "source": "herdr_core_hello",
                "route": "herdr_core",
                "forwardable": hc_forwarding_refusal(method.trim()).is_none(),
            })
        })
        .collect();
    let mut local: Vec<Value> = crate::progressive_skills::local_method_schemas(query)
        .into_iter()
        .map(|mut schema| {
            if let Some(object) = schema.as_object_mut() {
                object.insert("route".to_owned(), json!("workstation_local"));
            }
            schema
        })
        .collect();
    methods.append(&mut local);
    json!({
        "ok": true,
        "count": methods.len(),
        "methods": methods,
        "source": "herdr-core hello.methods (dynamically negotiated)",
        "local_method_source": "herdr_mcp_local_registry",
        "runtime_backend": "herdr-core",
    })
}

/// `herdr_fs_read` -> `file.read`. Accepts the TUI aliases (`offset`/`start_line`,
/// `limit`/`max_lines`, `end_line`) so one caller shape works on both backends.
///
/// The `path`/`cwd` pair is resolved to one absolute target before the secret
/// guard runs, and that target — not the caller's spelling of it — is what is
/// sent to `hc` (#936).
pub fn herdr_core_fs_read(client: &HerdrCoreClient, args: &Value) -> Value {
    const OPERATION: &str = "herdr_fs_read";
    let path = match hc_required_text(args, "path", OPERATION) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let cwd = match hc_arg_cwd(args, OPERATION) {
        Ok(cwd) => cwd,
        Err(error) => return error,
    };
    let target = match herdr_core_absolute_file_target(path, cwd.as_deref()) {
        Ok(target) => target,
        Err(error) => return herdr_core_error_json(&error, OPERATION, "not_delivered"),
    };
    let offset = match hc_first_u64(args, &["offset", "start_line"]) {
        Ok(offset) => offset,
        Err(error) => return error,
    };
    let mut limit = match hc_first_u64(args, &["limit", "max_lines"]) {
        Ok(limit) => limit,
        Err(error) => return error,
    };
    if limit.is_none()
        && let Some(end_line) = args.get("end_line").and_then(Value::as_u64)
    {
        let start = offset.unwrap_or(1);
        if end_line < start {
            return hc_invalid_params_value("end_line must be >= start_line");
        }
        limit = Some(end_line.saturating_sub(start).saturating_add(1));
    }
    match client.file_read(&target, offset, limit) {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                object.insert("content".to_owned(), json!(text));
            }
            value
        }
        Err(error) => herdr_core_error_json(&error, OPERATION, hc_delivery_state(&error, false)),
    }
}

/// `herdr_fs_write` -> `file.write`. The public route is currently blocked.
/// Lease and target resolution occur at different times and path races remain.
pub fn herdr_core_fs_write(client: &HerdrCoreClient, args: &Value) -> Value {
    const OPERATION: &str = "herdr_fs_write";
    let path = match hc_required_text(args, "path", OPERATION) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let content = match hc_required_raw_text(args, "content", OPERATION) {
        Ok(content) => content,
        Err(error) => return error,
    };
    let cwd = match hc_arg_cwd(args, OPERATION) {
        Ok(cwd) => cwd,
        Err(error) => return error,
    };
    let target = match herdr_core_absolute_file_target(path, cwd.as_deref()) {
        Ok(target) => target,
        Err(error) => return herdr_core_error_json(&error, OPERATION, "not_delivered"),
    };
    match client.file_write(&target, content.as_bytes()) {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("content_bytes".to_owned(), json!(content.len()));
            }
            value
        }
        Err(error) => herdr_core_error_json(&error, OPERATION, hc_delivery_state(&error, true)),
    }
}

/// `herdr_fs_edit` -> `file.edit`. Accepts `old_text`/`new_text`,
/// `old_string`/`new_string`, or an `edits[]` array of either shape; every edit
/// is all-or-none in `hc`.
pub fn herdr_core_fs_edit(client: &HerdrCoreClient, args: &Value) -> Value {
    const OPERATION: &str = "herdr_fs_edit";
    let path = match hc_required_text(args, "path", OPERATION) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let cwd = match hc_arg_cwd(args, OPERATION) {
        Ok(cwd) => cwd,
        Err(error) => return error,
    };
    let target = match herdr_core_absolute_file_target(path, cwd.as_deref()) {
        Ok(target) => target,
        Err(error) => return herdr_core_error_json(&error, OPERATION, "not_delivered"),
    };
    let mut edits: Vec<(String, String)> = Vec::new();
    match args.get("edits") {
        None | Some(Value::Null) => {
            let old = hc_first_text(args, &["old_text", "old_string"]);
            let new = hc_first_text(args, &["new_text", "new_string"]);
            match (old, new) {
                (Some(old), Some(new)) => edits.push((old.to_owned(), new.to_owned())),
                (None, _) => {
                    return hc_invalid_params_value(
                        "herdr_fs_edit requires old_text/old_string (or an edits array)",
                    );
                }
                (Some(_), None) => {
                    return hc_invalid_params_value(
                        "herdr_fs_edit requires new_text/new_string next to old_text/old_string",
                    );
                }
            }
        }
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return hc_invalid_params_value("edits must contain at least one replacement");
            }
            for (index, item) in items.iter().enumerate() {
                let old = hc_first_text(item, &["old", "old_text", "old_string"]);
                let new = hc_first_text(item, &["new", "new_text", "new_string"]);
                match (old, new) {
                    (Some(old), Some(new)) => edits.push((old.to_owned(), new.to_owned())),
                    _ => {
                        return hc_invalid_params_value(format!(
                            "edits[{index}] requires old/old_text and new/new_text"
                        ));
                    }
                }
            }
        }
        Some(_) => return hc_invalid_params_value("edits must be an array"),
    }
    if edits.iter().any(|(old, _)| old.is_empty()) {
        return hc_invalid_params_value("old_text/old_string must not be empty");
    }
    match client.file_edit(&target, &edits) {
        Ok(value) => value,
        Err(error) => herdr_core_error_json(&error, OPERATION, hc_delivery_state(&error, true)),
    }
}

/// `herdr_exec` -> `machine.exec` (`line` for a shell command, `argv` for a
/// typed program). `root`/`project_root`/`cwd` becomes the `cwd`.
pub fn herdr_core_exec(client: &HerdrCoreClient, args: &Value) -> Value {
    const OPERATION: &str = "herdr_exec";
    if let Err(error) = hc_mutation_guard(OPERATION) {
        return herdr_core_error_json(&error, OPERATION, "not_delivered");
    }
    let invocation = match hc_exec_invocation(args, OPERATION) {
        Ok(invocation) => invocation,
        Err(error) => return error,
    };
    let cwd = match hc_arg_cwd(args, OPERATION) {
        Ok(cwd) => cwd,
        Err(error) => return error,
    };
    let Some(cwd) = cwd else {
        return hc_invalid_params_value(
            "herdr-core herdr_exec requires root, project_root or cwd: the workspace/pane graph that resolves a cwd on the TUI backend does not exist on herdr-core",
        );
    };
    let intent = match hc_exec_request_gate(args, OPERATION) {
        Ok(intent) => intent,
        Err(error) => return error,
    };
    let timeout_ms = match hc_arg_u64(args, "timeout_ms") {
        Ok(timeout_ms) => timeout_ms.unwrap_or(30_000),
        Err(error) => return error,
    };
    if !(1..=60_000).contains(&timeout_ms) {
        return hc_invalid_params_value("timeout_ms must be an integer in 1..=60000");
    }
    let timeout_secs = timeout_ms.div_ceil(1000).clamp(1, 600);
    let executed = match &invocation {
        HcExecInvocation::Line(line) => {
            client.machine_exec_line(line, Some(&cwd), Some(timeout_secs))
        }
        HcExecInvocation::Argv(argv) => client.machine_exec(argv, Some(&cwd), Some(timeout_secs)),
    };
    match executed {
        Ok(mut value) => {
            let stdout = value
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let stderr = value
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let output = if stderr.is_empty() {
                stdout
            } else {
                format!("{stdout}{stderr}")
            };
            if let Some(object) = value.as_object_mut() {
                object.insert("output".to_owned(), json!(output));
                object.insert("command".to_owned(), json!(invocation.rendered()));
                object.insert(
                    "execution".to_owned(),
                    json!({
                        "method": "machine.exec",
                        "mode": invocation.mode(),
                        "cwd": cwd.to_string_lossy(),
                        "timeout_secs": timeout_secs,
                    }),
                );
                if let Some(intent) = intent {
                    object.insert("intent".to_owned(), json!(intent));
                }
            }
            value
        }
        Err(error) => herdr_core_error_json(&error, OPERATION, hc_delivery_state(&error, true)),
    }
}

/// `herdr_exec_start` is refused on `herdr-core` (#936).
///
/// `terminal.run` creates a pty terminal in one address namespace shared with
/// every unrelated agent terminal, and `hc` offers no runtime-scoped session id,
/// ownership marker or byte cursor. Starting one would create a session that no
/// public route can safely read or end, so the route fails closed instead of
/// inventing an identity. The one-shot `herdr_exec` route also fails closed
/// until public tool policy and result contract parity is available.
pub fn herdr_core_exec_start(_client: &HerdrCoreClient, _args: &Value) -> Value {
    herdr_core_exec_session_unsupported("herdr_exec_start")
}

/// `herdr_exec_read` is refused on `herdr-core` (#936).
///
/// The public contract addresses a session by `session_id` and reads a byte
/// cursor. `hc terminal.read` takes a bare terminal address — which may be an
/// unrelated agent terminal — and returns screen/history text with no cursor, so
/// neither ownership nor the cursor contract can be preserved.
pub fn herdr_core_exec_read(_client: &HerdrCoreClient, _args: &Value) -> Value {
    herdr_core_exec_session_unsupported("herdr_exec_read")
}

/// `herdr_exec_kill` is refused on `herdr-core` (#936).
///
/// `hc terminal.kill` accepts any address in the shared terminal namespace, so a
/// caller-supplied `session_id` could end another agent's work. Ownership cannot
/// be proven without inventing identity, so the route fails closed.
pub fn herdr_core_exec_kill(_client: &HerdrCoreClient, _args: &Value) -> Value {
    herdr_core_exec_session_unsupported("herdr_exec_kill")
}

/// The herdr-core transport cannot preserve prompt task tracking and identity.
/// Refuse every prompt before attempting delivery.
pub fn herdr_core_prompt(_client: &HerdrCoreClient, _args: &Value) -> Value {
    herdr_core_unsupported(
        "herdr_prompt",
        "prompt_task_tracking_and_identity",
        "herdr-core cannot preserve herdr_prompt task tracking and identity; the prompt was not delivered",
    )
}

/// `herdr_call` forwarding under `herdr-core`.
///
/// Local `herdr_mcp.*` methods never reach here. The method name is trimmed
/// before any decision, so leading whitespace cannot slip past the `machine.*`
/// fence (#936 P1-2). Raw forwarding is then limited to the precise read-only
/// allowlist in [`HERDR_CALL_FORWARDABLE_HC_METHODS`]; every file/machine/
/// terminal primitive fails closed with `herdr_core_method_prohibited` and
/// cannot bypass the
/// single-writer root lease, `HERDR_MCP_READONLY`, the secret-path denylist or
/// the prompt human boundary (#936 P0-1). Methods that are allowlisted but not
/// advertised by `hello.methods` still fail closed with
/// `runtime_capability_unsupported`.
pub fn herdr_core_call(client: &HerdrCoreClient, method: &str, params: Value) -> Value {
    let method = method.trim();
    if let Some(message) = hc_forwarding_refusal(method) {
        return json!({
            "ok": false,
            "code": "herdr_core_method_prohibited",
            "method": method,
            "runtime_backend": "herdr-core",
            "retryable": false,
            "fallback_allowed": false,
            "delivery_state": "not_delivered",
            "message": message,
        });
    }
    match client.call_allowed_method(method, params) {
        Ok(result) => json!({
            "ok": true,
            "runtime_backend": "herdr-core",
            "method": method,
            "result": result,
        }),
        Err(error) => herdr_core_error_json(&error, method, hc_delivery_state(&error, true)),
    }
}

#[derive(Debug, Clone)]
enum HcExecInvocation {
    Line(String),
    Argv(Vec<String>),
}

impl HcExecInvocation {
    const fn mode(&self) -> &'static str {
        match self {
            Self::Line(_) => "line",
            Self::Argv(_) => "argv",
        }
    }

    fn rendered(&self) -> String {
        match self {
            Self::Line(line) => line.clone(),
            Self::Argv(argv) => argv.join(" "),
        }
    }
}

fn hc_exec_invocation(args: &Value, operation: &str) -> Result<HcExecInvocation, Value> {
    if args.get("steps").is_some_and(|steps| !steps.is_null()) {
        return Err(herdr_core_unsupported(
            operation,
            "exec_steps",
            "herdr-core does not support exec steps; the request was not delivered",
        ));
    }
    let command = hc_first_text(args, &["command"]);
    let program = hc_first_text(args, &["program"]);
    let raw_args = args.get("args");
    if command.is_some() && (program.is_some() || raw_args.is_some()) {
        return Err(hc_invalid_params_value(
            "command and program/args modes are mutually exclusive",
        ));
    }
    if let Some(command) = command {
        if command.trim().is_empty() {
            return Err(hc_invalid_params_value("command must not be empty"));
        }
        return Ok(HcExecInvocation::Line(command.to_owned()));
    }
    let Some(program) = program else {
        if raw_args.is_some() {
            return Err(hc_invalid_params_value(
                "program is required when args is provided",
            ));
        }
        return Err(hc_invalid_params_value(format!(
            "{operation} requires command or program/args"
        )));
    };
    if program.is_empty() || program.contains('\0') {
        return Err(hc_invalid_params_value(
            "program must be non-empty and contain no NUL",
        ));
    }
    let mut argv = vec![program.to_owned()];
    match raw_args {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) => {
            if values.len() > 128 {
                return Err(hc_invalid_params_value(
                    "args must contain at most 128 strings",
                ));
            }
            for value in values {
                let Some(value) = value.as_str() else {
                    return Err(hc_invalid_params_value("args must be an array of strings"));
                };
                if value.contains('\0') {
                    return Err(hc_invalid_params_value(
                        "args must contain only NUL-free strings",
                    ));
                }
                argv.push(value.to_owned());
            }
        }
        Some(_) => return Err(hc_invalid_params_value("args must be an array of strings")),
    }
    Ok(HcExecInvocation::Argv(argv))
}

/// `herdr_exec` intent/idempotency contract, validated explicitly under
/// `herdr-core`. `hc machine.exec` has no durable replay store, so a write intent
/// that would need one fails closed instead of pretending the key was honoured.
fn hc_exec_request_gate(args: &Value, operation: &str) -> Result<Option<&'static str>, Value> {
    let intent = match args.get("intent").and_then(Value::as_str) {
        None => None,
        Some("") => return Err(hc_invalid_params_value("intent must not be empty")),
        Some(value) => match value {
            "read_only" => Some("read_only"),
            "idempotent_write" => Some("idempotent_write"),
            "non_idempotent_write" => Some("non_idempotent_write"),
            _ => {
                return Err(hc_invalid_params_value(
                    "intent must be one of read_only, idempotent_write, non_idempotent_write",
                ));
            }
        },
    };
    let key = match args.get("idempotency_key") {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) if key.is_empty() => {
            return Err(hc_invalid_params_value("idempotency_key must not be empty"));
        }
        Some(Value::String(key)) if key.chars().count() > 256 => {
            return Err(hc_invalid_params_value(
                "idempotency_key must be at most 256 characters",
            ));
        }
        Some(Value::String(key)) => Some(key.as_str()),
        Some(_) => return Err(hc_invalid_params_value("idempotency_key must be a string")),
    };
    match (intent, key) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(hc_invalid_params_value(
            "idempotency_key requires an explicit intent (read_only, idempotent_write, or non_idempotent_write)",
        )),
        (Some(_), None) if intent != Some("read_only") => Err(hc_invalid_params_value(
            "a write intent requires idempotency_key so the request can be replayed durably",
        )),
        (Some(_), Some(_)) => Err(herdr_core_unsupported(
            operation,
            "exec_durable_idempotency",
            "herdr-core machine.exec has no durable idempotency store, so a keyed exec is refused instead of silently losing replay protection; omit intent/idempotency_key, or declare intent=read_only",
        )),
        (Some(intent), None) => Ok(Some(intent)),
    }
}

fn hc_arg_text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn hc_first_text<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| hc_arg_text(args, key))
}

fn hc_first_trimmed<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| hc_arg_text(args, key))
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn hc_required_text<'a>(args: &'a Value, key: &str, operation: &str) -> Result<&'a str, Value> {
    match hc_arg_text(args, key) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        Some(_) => Err(hc_invalid_params_value(format!(
            "{operation} requires a non-empty '{key}'"
        ))),
        None => Err(hc_invalid_params_value(format!(
            "{operation} requires '{key}' as a string"
        ))),
    }
}

fn hc_required_raw_text<'a>(args: &'a Value, key: &str, operation: &str) -> Result<&'a str, Value> {
    match hc_arg_text(args, key) {
        Some(value) => Ok(value),
        None => Err(hc_invalid_params_value(format!(
            "{operation} requires '{key}' as a string"
        ))),
    }
}

fn hc_arg_u64(args: &Value, key: &str) -> Result<Option<u64>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            hc_invalid_params_value(format!("{key} must be a non-negative integer"))
        }),
    }
}

fn hc_first_u64(args: &Value, keys: &[&str]) -> Result<Option<u64>, Value> {
    for key in keys {
        if let Some(value) = hc_arg_u64(args, key)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Resolve the absolute `cwd` for a `herdr-core` route (`root` ->
/// `project_root` -> `cwd`). The affected public routes are currently blocked;
/// route and lease resolution occur at different times, so path races remain.
///
/// A relative directory is refused rather than forwarded: `hc` would resolve it
/// against the herdr-core process cwd, not this process's (#936).
fn hc_arg_cwd(args: &Value, operation: &str) -> Result<Option<PathBuf>, Value> {
    // Every supplied directory is resolved, not only the winning one: a shadowed
    // relative `cwd` is still ambiguous, and the root lease checks it too (#936).
    let mut resolved: Option<PathBuf> = None;
    for key in ["root", "project_root", "cwd"] {
        let Some(value) = hc_first_trimmed(args, &[key]) else {
            continue;
        };
        let directory = match herdr_core_absolute_directory(value) {
            Ok(directory) => directory,
            Err(error) => return Err(herdr_core_error_json(&error, operation, "not_delivered")),
        };
        if resolved.is_none() {
            resolved = Some(directory);
        }
    }
    if resolved.is_some() {
        return Ok(resolved);
    }
    if let Some(workspace) = hc_first_trimmed(args, &["workspace"]) {
        return Err(hc_invalid_params_value(format!(
            "herdr-core has no workspace graph, so workspace '{workspace}' cannot resolve a cwd; pass root, project_root or cwd"
        )));
    }
    Ok(None)
}

fn hc_invalid_params_value(message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "code": "invalid_params",
        "runtime_backend": "herdr-core",
        "retryable": false,
        "fallback_allowed": false,
        "delivery_state": "not_delivered",
        "message": message.into(),
    })
}

/// Error codes refused before anything is delivered to `hc`.
const HC_PREFLIGHT_ERROR_CODES: &[&str] = &[
    "invalid_params",
    "runtime_capability_unsupported",
    "readonly_mode",
    "secret_path_denied",
    "herdr_core_path_ambiguous",
    "file_too_large",
    "edit_too_large",
    "herdr_core_method_prohibited",
    "human_boundary_prohibited",
    "human_boundary_blocked",
];

fn hc_delivery_state(error: &HerdrCoreError, mutating: bool) -> &'static str {
    if !mutating || HC_PREFLIGHT_ERROR_CODES.contains(&error.code.as_str()) {
        "not_delivered"
    } else {
        // A transport failure after the request left this process may already
        // have been applied; never claim `not_delivered`.
        "unknown"
    }
}

pub(crate) fn herdr_core_error_json(
    error: &HerdrCoreError,
    operation: &str,
    delivery_state: &str,
) -> Value {
    let mut payload = json!({
        "ok": false,
        "code": error.code,
        "operation": operation,
        "runtime_backend": "herdr-core",
        "message": error.message,
        "retryable": false,
        "fallback_allowed": false,
        "delivery_state": delivery_state,
    });
    if let Some(object) = payload.as_object_mut() {
        if let Some(kind) = &error.kind {
            object.insert("kind".to_owned(), json!(kind));
        }
        if error.code == "runtime_capability_unsupported" {
            object.insert("required_capability".to_owned(), json!("herdr_core_method"));
            object.insert("failure_phase".to_owned(), json!("backend_capability"));
        }
    }
    payload
}

pub fn inspect_herdr_core(selection: &RuntimeBackendSelection) -> Value {
    let Some(socket_path) = selection.herdr_core_socket.as_deref() else {
        return json!({
            "ok": false,
            "code": "herdr_core_socket_unconfigured",
            "message": "herdr-core backend selected without a resolved socket path",
            "runtime_backend": selection.summary_json(),
            "fallback_allowed": false,
            "context": "herdr_inspect",
        });
    };
    let client = HerdrCoreClient::new(socket_path);
    match client.inspect_terminals() {
        Ok(mut output) => {
            if let Some(obj) = output.as_object_mut() {
                obj.insert("runtime_backend".to_owned(), selection.summary_json());
            }
            output
        }
        Err(error) => json!({
            "ok": false,
            "code": error.code,
            "kind": error.kind,
            "message": error.message,
            "runtime_backend": selection.summary_json(),
            "fallback_allowed": false,
            "context": "herdr_inspect",
            "failure_phase": "herdr_core_handshake",
        }),
    }
}

pub fn doctor_report_json(config: &Config) -> Value {
    match resolve_backend_selection(config) {
        Ok(selection) => {
            let mut base = selection.summary_json();
            if selection.backend == RuntimeBackend::HerdrCore
                && let Some(socket_path) = selection.herdr_core_socket.as_deref()
            {
                let session = HerdrCoreClient::new(socket_path).status_summary();
                if let Some(obj) = base.as_object_mut() {
                    obj.insert("herdr_core_session".to_owned(), session);
                }
            }
            base
        }
        Err(error) => json!({
            "selected": "invalid",
            "source": "env",
            "fallback_allowed": false,
            "error": error,
        }),
    }
}

pub fn status_line(config: &Config) -> String {
    match resolve_backend_selection(config) {
        Ok(selection) => format!(
            "{} (source={}, fallback=disabled)",
            selection.backend.as_str(),
            selection.source
        ),
        Err(error) => format!("invalid ({error})"),
    }
}

#[cfg(unix)]
type HcSocketStream = std::os::unix::net::UnixStream;

#[cfg(not(unix))]
type HcSocketStream = std::io::Empty;

#[cfg(unix)]
fn connect_hc_socket(
    socket_path: &Path,
    timeout: Duration,
) -> Result<HcSocketStream, HerdrCoreError> {
    let stream = std::os::unix::net::UnixStream::connect(socket_path)
        .map_err(|error| map_hc_io_error(error, socket_path))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| map_hc_io_error(error, socket_path))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| map_hc_io_error(error, socket_path))?;
    Ok(stream)
}

#[cfg(not(unix))]
fn connect_hc_socket(
    socket_path: &Path,
    _timeout: Duration,
) -> Result<HcSocketStream, HerdrCoreError> {
    Err(HerdrCoreError {
        code: "herdr_core_windows_ipc_unverified".to_owned(),
        kind: None,
        message: format!(
            "herdr-core Windows local IPC transport and ACL model are not yet verified ({})",
            socket_path.display()
        ),
    })
}

fn rpc_call_line(
    stream: &mut HcSocketStream,
    socket_path: &Path,
    method: &str,
    params: Value,
    call_timeout: Duration,
) -> Result<Value, HerdrCoreError> {
    let id = NEXT_HC_RPC_ID.fetch_add(1, Ordering::Relaxed);
    // One monotonic deadline for the whole request/reply exchange. Per-`read`
    // timeouts are derived from the remaining time, so a peer that drips bytes
    // cannot restart the budget on every read.
    let deadline = Instant::now() + call_timeout;
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let mut encoded = serde_json::to_vec(&request).map_err(|error| HerdrCoreError {
        code: "encode_error".to_owned(),
        kind: None,
        message: error.to_string(),
    })?;
    encoded.push(b'\n');
    hc_apply_write_budget(stream, socket_path, deadline)?;
    stream
        .write_all(&encoded)
        .map_err(|error| map_hc_io_error(error, socket_path))?;

    let line = read_hc_line(stream, socket_path, deadline, call_timeout)?;
    let envelope: Value = serde_json::from_slice(&line).map_err(|error| HerdrCoreError {
        code: "parse_error".to_owned(),
        kind: None,
        message: error.to_string(),
    })?;

    // Fail closed on a mismatched or missing id: a reordered/crossed reply must
    // never be reported as the result of this request.
    if envelope.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(HerdrCoreError {
            code: "herdr_core_response_id_mismatch".to_owned(),
            kind: Some("response_id_mismatch".to_owned()),
            message: format!(
                "herdr-core reply for '{method}' carried id {:?}, expected {id}",
                envelope.get("id").cloned().unwrap_or(Value::Null)
            ),
        });
    }

    if let Some(error) = envelope.get("error") {
        let kind = error
            .get("data")
            .and_then(|d| d.get("kind"))
            .and_then(Value::as_str)
            .or_else(|| error.get("kind").and_then(Value::as_str))
            .map(str::to_owned);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("herdr-core RPC error")
            .to_owned();
        // `hc` reports every domain failure as code -32000 with a stable
        // `data.kind`; the kind (not the numeric code) decides the typed error so
        // `edit_failed`, `not_found`, `blocked`, ... stay distinguishable.
        let code = if kind.as_deref() == Some("unsupported_generation") {
            "unsupported_generation".to_owned()
        } else if let Some(ref k) = kind {
            format!("herdr_core_{k}")
        } else {
            "herdr_core_rpc_error".to_owned()
        };
        return Err(HerdrCoreError {
            code,
            kind,
            message,
        });
    }

    Ok(envelope.get("result").cloned().unwrap_or_else(|| json!({})))
}

/// Read one newline-delimited reply in chunks under a single total deadline.
///
/// Before every chunk the socket read timeout is re-armed with the remaining
/// budget, so the deadline is monotonic across the whole call rather than a
/// per-read idle timeout. Bytes after the terminating newline are discarded: this
/// client sends one request at a time and never subscribes to event streams, so a
/// peer has nothing else to say on the connection.
fn read_hc_line(
    stream: &mut HcSocketStream,
    socket_path: &Path,
    deadline: Instant,
    call_timeout: Duration,
) -> Result<Vec<u8>, HerdrCoreError> {
    let mut output = Vec::with_capacity(1024);
    let mut chunk = [0_u8; HC_READ_CHUNK_BYTES];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(hc_deadline_exceeded(socket_path, call_timeout));
        }
        hc_set_stream_read_timeout(stream, remaining)
            .map_err(|error| map_hc_io_error(error, socket_path))?;
        let count = stream.read(&mut chunk).map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                hc_deadline_exceeded(socket_path, call_timeout)
            } else {
                HerdrCoreError {
                    code: "herdr_core_socket_error".to_owned(),
                    kind: None,
                    message: error.to_string(),
                }
            }
        })?;
        if count == 0 {
            return Err(HerdrCoreError {
                code: "unexpected_eof".to_owned(),
                kind: None,
                message: "herdr-core socket closed before newline-delimited JSON-RPC reply"
                    .to_owned(),
            });
        }
        match chunk[..count].iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                output.extend_from_slice(&chunk[..newline]);
                if output.len() > HC_MAX_LINE_BYTES {
                    return Err(hc_response_too_large());
                }
                return Ok(output);
            }
            None => output.extend_from_slice(&chunk[..count]),
        }
        if output.len() > HC_MAX_LINE_BYTES {
            return Err(hc_response_too_large());
        }
    }
}

fn hc_response_too_large() -> HerdrCoreError {
    HerdrCoreError {
        code: "response_too_large".to_owned(),
        kind: None,
        message: format!("herdr-core reply exceeded {HC_MAX_LINE_BYTES} bytes"),
    }
}

fn hc_deadline_exceeded(socket_path: &Path, call_timeout: Duration) -> HerdrCoreError {
    HerdrCoreError {
        code: "timeout".to_owned(),
        kind: Some("timeout".to_owned()),
        message: format!(
            "{}: no complete herdr-core reply within the {:.1}s call budget",
            socket_path.display(),
            call_timeout.as_secs_f64()
        ),
    }
}

#[cfg(unix)]
fn hc_set_stream_read_timeout(
    stream: &mut HcSocketStream,
    remaining: Duration,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))
}

#[cfg(not(unix))]
fn hc_set_stream_read_timeout(
    _stream: &mut HcSocketStream,
    _remaining: Duration,
) -> std::io::Result<()> {
    Ok(())
}

fn hc_apply_write_budget(
    stream: &mut HcSocketStream,
    socket_path: &Path,
    deadline: Instant,
) -> Result<(), HerdrCoreError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(hc_deadline_exceeded(socket_path, Duration::ZERO));
    }
    #[cfg(unix)]
    stream
        .set_write_timeout(Some(remaining.max(Duration::from_millis(1))))
        .map_err(|error| map_hc_io_error(error, socket_path))?;
    #[cfg(not(unix))]
    let _ = (stream, socket_path);
    Ok(())
}

fn map_hc_io_error(error: std::io::Error, socket_path: &Path) -> HerdrCoreError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "herdr_core_unreachable"
        }
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timeout",
        _ => "herdr_core_socket_error",
    };
    HerdrCoreError {
        code: code.to_owned(),
        kind: None,
        message: format!("{}: {error}", socket_path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn non_unicode_session_is_checked_only_when_socket_needs_it() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::process::Command;
        const CHILD: &str = "HERDR_TEST_SESSION_PRIORITY";
        if let Ok(case) = std::env::var(CHILD) {
            let mut config = Config::default();
            if case == "config-socket" {
                config.runtime_herdr_core_socket = Some("/isolated/config.sock".to_owned());
            }
            let from_process = resolve_process_backend_selection();
            let from_config = resolve_backend_selection(&config);
            for result in [from_process, from_config] {
                match case.as_str() {
                    "tui" => assert_eq!(result.unwrap().backend, RuntimeBackend::Tui),
                    "config-socket" => assert_eq!(
                        result.unwrap().herdr_core_socket.as_deref(),
                        Some(Path::new("/isolated/config.sock"))
                    ),
                    "env-socket" => assert_eq!(
                        result.unwrap().herdr_core_socket.as_deref(),
                        Some(Path::new("/isolated/env.sock"))
                    ),
                    "session-required" => assert!(
                        result
                            .unwrap_err()
                            .contains("HC_SESSION must be valid UTF-8")
                    ),
                    _ => panic!("unexpected child case"),
                }
            }
            return;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-session-priority-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        for case in ["tui", "config-socket", "env-socket", "session-required"] {
            let dir = root.join(case);
            let config_dir = dir.join("config");
            std::fs::create_dir_all(&config_dir).unwrap();
            if case == "config-socket" {
                std::fs::write(config_dir.join("config.json"), r#"{"runtime":{"backend":"herdr-core","herdr_core_socket":"/isolated/config.sock"}}"#).unwrap();
            }
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "runtime_adapter::tests::non_unicode_session_is_checked_only_when_socket_needs_it", "--nocapture"])
                .env_clear()
                .env("HOME", &dir)
                .env("TMPDIR", &dir)
                .env("HERDR_MCP_CONFIG_DIR", &config_dir)
                .env(CHILD, case)
                .env("HC_SESSION", OsString::from_vec(vec![0xff]));
            if case != "tui" {
                child.env(RUNTIME_BACKEND_ENV, "herdr-core");
            }
            if case == "config-socket" || case == "env-socket" {
                child.env("HC_SOCKET", "/isolated/env.sock");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "child for {case} failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child did not execute targeted test"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_runtime_overrides_are_rejected_by_process_resolvers() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::process::Command;
        const CHILD: &str = "HERDR_TEST_NON_UNICODE_OVERRIDE";
        if let Ok(name) = std::env::var(CHILD) {
            let error = resolve_process_backend_selection().unwrap_err();
            assert!(error.contains(&name), "unexpected error: {error}");
            let error = resolve_backend_selection(&Config::default()).unwrap_err();
            assert!(error.contains(&name), "unexpected error: {error}");
            return;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-invalid-env-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for name in [RUNTIME_BACKEND_ENV, "HC_SESSION"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "runtime_adapter::tests::non_unicode_runtime_overrides_are_rejected_by_process_resolvers", "--nocapture"])
                .env_clear()
                .env("HOME", &root)
                .env("TMPDIR", &root)
                .env("HERDR_MCP_CONFIG_DIR", root.join("config"))
                .env(CHILD, name)
                .env(name, OsString::from_vec(vec![0xff]));
            if name == "HC_SESSION" {
                child.env(RUNTIME_BACKEND_ENV, "herdr-core");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "child for {name} failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child did not execute the targeted test"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_backed_selection_uses_real_files_and_fails_closed() {
        use std::fs;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-selection-{}-{unique}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("config.json");
        let legacy = root.join("config.toml");
        let instance = crate::instance::InstanceId::parse("uat").unwrap();
        let env = EnvOverrides::default();
        let select = || resolve_backend_selection_from_path(&path, &instance, &env);
        assert_eq!(select().unwrap().backend, RuntimeBackend::Tui);
        fs::write(&legacy, "[runtime]\nport = 9000\n").unwrap();
        assert_eq!(select().unwrap().backend, RuntimeBackend::Tui);
        assert!(!path.exists(), "admission must not migrate legacy config");
        fs::write(
            &path,
            r#"{"runtime":{"backend":"herdr-core","herdr_core_socket":"/test/one.sock"}}"#,
        )
        .unwrap();
        let selected = select().unwrap();
        assert_eq!(selected.backend, RuntimeBackend::HerdrCore);
        assert_eq!(
            selected.herdr_core_socket.as_deref(),
            Some(Path::new("/test/one.sock"))
        );
        fs::write(&path, r#"{"runtime":{"backend":"auto"}}"#).unwrap();
        assert!(
            select().is_err(),
            "invalid modern config cannot fall back to legacy TUI"
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            select().is_err(),
            "unreadable modern config cannot fall back to legacy TUI"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn public_core_policy_gaps_fail_closed_before_contact_and_tui_stays_admitted() {
        use std::os::unix::net::UnixListener;

        let socket = std::env::temp_dir().join(format!(
            "herdr-policy-admission-{}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let core = RuntimeBackendSelection {
            backend: RuntimeBackend::HerdrCore,
            source: "test",
            herdr_core_socket: Some(socket.clone()),
        };
        let tui = RuntimeBackendSelection {
            backend: RuntimeBackend::Tui,
            source: "test",
            herdr_core_socket: None,
        };
        for tool in [
            "herdr_fs_read",
            "herdr_fs_write",
            "herdr_fs_edit",
            "herdr_exec",
        ] {
            let rejection = match public_tool_route_for_selection(tool, &core) {
                Err(rejection) => rejection,
                Ok(_) => panic!("{tool} was admitted on herdr-core"),
            };
            assert_eq!(rejection["ok"], false);
            assert_eq!(rejection["code"], "runtime_capability_unsupported");
            assert_eq!(rejection["operation"], tool);
            assert_eq!(
                rejection["required_capability"],
                "public_tool_policy_parity"
            );
            assert_eq!(rejection["delivery_state"], "not_delivered");
            assert_eq!(rejection["retryable"], false);
            assert_eq!(rejection["fallback_allowed"], false);
            assert!(matches!(
                public_tool_route_for_selection(tool, &tui),
                Ok(PublicToolRoute::Tui)
            ));
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(listener);
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn explicit_backend_selection_never_falls_back_on_unknown_values() {
        assert_eq!(
            RuntimeBackend::parse_explicit("tui").unwrap(),
            RuntimeBackend::Tui
        );
        assert_eq!(
            RuntimeBackend::parse_explicit("herdr-core").unwrap(),
            RuntimeBackend::HerdrCore
        );
        assert!(RuntimeBackend::parse_explicit("auto").is_err());
        assert!(RuntimeBackend::parse_explicit("fallback").is_err());

        let bad_env = EnvOverrides {
            runtime_backend: Some("herdr-app".to_owned()),
            ..EnvOverrides::default()
        };
        let err = resolve_backend_selection_with_env(None, false, &bad_env).unwrap_err();
        assert!(err.contains("no fallback"));
    }

    #[test]
    fn socket_resolution_honors_config_then_env_then_xdg_session() {
        let env = EnvOverrides {
            runtime_backend: Some("herdr-core".to_owned()),
            hc_session: Some("uat768".into()),
            xdg_state_home: Some(PathBuf::from("/tmp/xdg-state-test")),
            ..EnvOverrides::default()
        };
        let resolved = resolve_backend_selection_with_env(None, false, &env).unwrap();
        assert_eq!(resolved.backend, RuntimeBackend::HerdrCore);
        assert_eq!(resolved.source, "env");
        assert_eq!(
            resolved.herdr_core_socket.unwrap(),
            PathBuf::from("/tmp/xdg-state-test/herdr-core/uat768/uat768.sock")
        );

        for rejected_session in ["../escape", ".", "..", "./escape", ""] {
            let traversal = EnvOverrides {
                runtime_backend: Some("herdr-core".to_owned()),
                hc_session: Some(rejected_session.into()),
                xdg_state_home: Some(PathBuf::from("/tmp/xdg-state-test")),
                ..EnvOverrides::default()
            };
            assert!(
                resolve_backend_selection_with_env(None, false, &traversal).is_err(),
                "HC_SESSION {rejected_session:?} must be rejected"
            );
        }
    }

    #[test]
    fn capability_negotiation_derives_dynamic_subset_and_fails_closed_for_missing_tools() {
        let methods: BTreeSet<String> = [
            "hello",
            "server.status",
            "terminal.list",
            "terminal.get",
            "terminal.read",
            "terminal.run",
            "terminal.kill",
            "terminal.send",
            "machine.exec",
            "file.read",
            "file.write",
            "file.edit",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let caps = RuntimeCapabilities::from_herdr_core_methods(&methods);
        assert!(caps.check_public_tool("herdr_skill").is_ok());
        assert!(caps.check_public_tool("herdr_inspect").is_ok());
        assert!(caps.check_public_tool("herdr_exec").is_ok());
        assert!(caps.check_public_tool("herdr_fs_read").is_ok());
        assert!(caps.check_public_tool("herdr_fs_edit").is_ok());
        assert!(caps.check_public_tool("herdr_prompt").is_ok());

        for unservable in [
            "herdr_fs_list",
            "herdr_fs_grep",
            "herdr_fs_image",
            "herdr_fs_patch",
            "herdr_git",
            "herdr_since",
        ] {
            let err = caps.check_public_tool(unservable).unwrap_err();
            assert_eq!(err["ok"], false);
            assert_eq!(err["code"], "runtime_capability_unsupported");
            assert_eq!(err["runtime_backend"], "herdr-core");
            assert_eq!(err["fallback_allowed"], false);
            assert_eq!(err["retryable"], false);
            assert_eq!(err["delivery_state"], "not_delivered");
        }
    }

    #[cfg(unix)]
    #[test]
    fn herdr_core_openrpc_handshake_exec_inspect_and_human_boundary_guard() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        use std::thread;

        let sock = std::env::temp_dir().join(format!(
            "herdr-core-mock-{}-{}.sock",
            std::process::id(),
            NEXT_HC_RPC_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let server = thread::spawn(move || {
            // Serve 4 sequential sessions: inspect, machine.exec, blocked terminal.send, prohibited method
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let id = req["id"].clone();
                    match req["method"].as_str().unwrap() {
                        "hello" => {
                            assert_eq!(req["params"]["generation"], 1);
                            assert!(req["params"]["calls_you"].is_null());
                            assert_eq!(req["params"]["framed"], false);
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "generation": 1,
                                        "version": "0.1.14",
                                        "core": { "id": "core_test", "name": "test-host" },
                                        "framed": false,
                                        "methods": [
                                            "hello",
                                            "server.status",
                                            "terminal.list",
                                            "terminal.get",
                                            "terminal.read",
                                            "terminal.run",
                                            "terminal.kill",
                                            "terminal.send",
                                            "machine.exec"
                                        ],
                                        "features": ["osc7501"]
                                    }
                                })
                            )
                            .unwrap();
                        }
                        "server.status" => {
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": { "live": 2, "finished": 0, "version": "0.1.14" }
                                })
                            )
                            .unwrap();
                        }
                        "terminal.list" => {
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "terminals": [
                                            {
                                                "address": "t1",
                                                "state": "live",
                                                "cwd": "/tmp/work",
                                                "argv": ["claude"],
                                                "labels": {"role": "worker"},
                                                "reported": true,
                                                "status": {"state": "idle"},
                                                "turn": 2
                                            },
                                            {
                                                "address": "t2",
                                                "state": "live",
                                                "cwd": "/tmp/plain",
                                                "argv": ["sh"],
                                                "labels": {},
                                                "reported": false,
                                                "status": {"state": "ignored"},
                                                "turn": 0
                                            }
                                        ]
                                    }
                                })
                            )
                            .unwrap();
                        }
                        "machine.exec" => {
                            let stdout_b64 =
                                base64::engine::general_purpose::STANDARD.encode(b"hello-hc\n");
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "code": 0,
                                        "stdout": stdout_b64,
                                        "stderr": "",
                                        "timed_out": false,
                                        "truncated": false,
                                        "ms": 4
                                    }
                                })
                            )
                            .unwrap();
                        }
                        "terminal.send" => {
                            // Verify force is hard-locked false!
                            assert_eq!(req["params"]["force"], false);
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {
                                        "code": -32009,
                                        "message": "t1 is blocked (waiting)",
                                        "data": { "kind": "blocked" }
                                    }
                                })
                            )
                            .unwrap();
                        }
                        other => panic!("unexpected method {other}"),
                    }
                }
            }
        });

        let client = HerdrCoreClient::new(&sock);
        let inspected = client.inspect_terminals().unwrap();
        assert_eq!(inspected["ok"], true);
        assert_eq!(inspected["herdr_core"]["version"], "0.1.14");
        assert_eq!(
            inspected["herdr_core"]["capabilities_scope"],
            "advertised herdr-core methods, not public tool availability"
        );
        let blocked = inspected["herdr_core"]["public_tools_policy_blocked"]
            .as_array()
            .unwrap();
        for name in [
            "herdr_fs_read",
            "herdr_exec",
            "herdr_exec_kill",
            "herdr_prompt",
        ] {
            assert!(blocked.contains(&json!(name)), "missing {name}");
        }
        assert_eq!(inspected["terminals"], json!([]));
        assert_eq!(
            inspected["terminal_details"],
            "unavailable_without_ownership"
        );
        assert_eq!(
            inspected["herdr_core"]["terminal_counts_scope"],
            "global, not visibility-filtered"
        );

        let exec_res = client
            .machine_exec(
                &["printf".to_owned(), "hello-hc\n".to_owned()],
                None,
                Some(5),
            )
            .unwrap();
        assert_eq!(exec_res["ok"], true);
        assert_eq!(exec_res["stdout"], "hello-hc\n");

        let send_err = client
            .terminal_send("t1", "continue", Some("idle"), false, Some(5))
            .unwrap_err();
        assert_eq!(send_err.code, "human_boundary_blocked");

        let prohibited = client
            .call_allowed_method("machine.add", json!({"name": "vps"}))
            .unwrap_err();
        assert_eq!(prohibited.code, "herdr_core_method_prohibited");

        let force_blocked = client
            .call_allowed_method(
                "terminal.send",
                json!({"terminal": "t1", "text": "x", "force": true}),
            )
            .unwrap_err();
        assert_eq!(force_blocked.code, "human_boundary_prohibited");

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// Typed `herdr-core` file/terminal primitives (#925) against a real Unix
    /// OpenRPC socket fake: the adapter must speak the `hc 0.1.20` wire schema and
    /// every guard must refuse before the socket is contacted.
    #[cfg(unix)]
    mod hc_primitives {
        use super::*;
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};
        use std::thread;

        static NEXT_MOCK: AtomicU64 = AtomicU64::new(0);

        type HcRecorder = Arc<Mutex<Vec<(String, Value)>>>;

        fn recorder() -> HcRecorder {
            Arc::new(Mutex::new(Vec::new()))
        }

        fn recorded(recorder: &HcRecorder, method: &str) -> Vec<Value> {
            recorder
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == method)
                .map(|(_, params)| params.clone())
                .collect()
        }

        /// A socket path that was never bound: any call that reaches it fails with
        /// `herdr_core_unreachable`, which is how these tests prove a guard ran first.
        fn absent_socket_path(tag: &str) -> PathBuf {
            std::env::temp_dir().join(format!(
                "herdr-core-absent-{tag}-{}-{}.sock",
                std::process::id(),
                NEXT_MOCK.fetch_add(1, Ordering::Relaxed)
            ))
        }

        #[derive(Debug, Clone, Copy, Default)]
        struct MockHcOptions {
            /// Advertise `framed: true` in `hello` (adapter must fail closed).
            framed: bool,
            /// Answer with `request_id + offset` to exercise id verification.
            reply_id_offset: u64,
        }

        struct MockHcServer {
            socket_path: PathBuf,
            handle: thread::JoinHandle<()>,
        }

        impl MockHcServer {
            fn start(
                tag: &str,
                sessions: usize,
                methods: &[&str],
                recorder: HcRecorder,
                reply: impl Fn(&str, &Value) -> Result<Value, Value> + Send + 'static,
            ) -> Self {
                Self::start_with(
                    MockHcOptions::default(),
                    tag,
                    sessions,
                    methods,
                    recorder,
                    reply,
                )
            }

            fn start_with(
                options: MockHcOptions,
                tag: &str,
                sessions: usize,
                methods: &[&str],
                recorder: HcRecorder,
                reply: impl Fn(&str, &Value) -> Result<Value, Value> + Send + 'static,
            ) -> Self {
                let socket_path = std::env::temp_dir().join(format!(
                    "herdr-core-mock-{tag}-{}-{}.sock",
                    std::process::id(),
                    NEXT_MOCK.fetch_add(1, Ordering::Relaxed)
                ));
                let _ = std::fs::remove_file(&socket_path);
                let listener = UnixListener::bind(&socket_path).unwrap();
                let methods: Vec<String> =
                    methods.iter().map(|method| (*method).to_owned()).collect();
                let handle = thread::spawn(move || {
                    for _ in 0..sessions {
                        let (mut stream, _) = listener.accept().unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap() == 0 {
                                break;
                            }
                            let request: Value = serde_json::from_str(&line).unwrap();
                            let id = request["id"].as_u64().unwrap() + options.reply_id_offset;
                            let request_id = request["id"].clone();
                            let method = request["method"].as_str().unwrap().to_owned();
                            let params = request["params"].clone();
                            let envelope = if method == "hello" {
                                assert_eq!(params["generation"], HERDR_CORE_PROTOCOL_GENERATION);
                                assert_eq!(params["framed"], false);
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "generation": HERDR_CORE_PROTOCOL_GENERATION,
                                        "version": "0.1.20",
                                        "core": { "id": "core_mock", "name": "mock-host" },
                                        "framed": options.framed,
                                        "methods": methods,
                                        "features": []
                                    }
                                })
                            } else {
                                assert_eq!(
                                    request_id.as_u64().unwrap() + options.reply_id_offset,
                                    id
                                );
                                recorder
                                    .lock()
                                    .unwrap()
                                    .push((method.clone(), params.clone()));
                                match reply(&method, &params) {
                                    Ok(result) => json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": result
                                    }),
                                    Err(error) => json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "error": error
                                    }),
                                }
                            };
                            // A client that abandoned the call (deadline) closes the
                            // connection; stop serving instead of panicking.
                            if writeln!(stream, "{envelope}").is_err() {
                                break;
                            }
                        }
                    }
                });
                Self {
                    socket_path,
                    handle,
                }
            }

            fn socket_path(&self) -> &Path {
                &self.socket_path
            }

            fn client(&self) -> HerdrCoreClient {
                HerdrCoreClient::new(&self.socket_path)
            }

            fn finish(self) {
                self.handle.join().unwrap();
                let _ = std::fs::remove_file(&self.socket_path);
            }
        }

        /// The absolute target that the resolvers produce for a caller's
        /// `cwd` + relative `path`, used by the wire-level tests below.
        fn target(cwd: &str, path: &str) -> PathBuf {
            super::herdr_core_absolute_file_target(path, Some(Path::new(cwd))).unwrap()
        }

        #[test]
        fn methods_distinguish_forwardable_from_advertised() {
            let server = MockHcServer::start(
                "methods-policy",
                1,
                &["hello", "server.status", "file.read"],
                recorder(),
                |_, _| panic!("inventory must not call a backend method"),
            );
            let result = super::herdr_core_methods(&server.client(), "");
            let methods = result["methods"].as_array().unwrap();
            for (name, expected) in [("server.status", true), ("file.read", false)] {
                let entry = methods.iter().find(|item| item["method"] == name).unwrap();
                assert_eq!(entry["forwardable"], expected, "{name}");
            }
            server.finish();
        }

        #[test]
        fn file_write_encodes_base64_and_file_read_maps_line_window() {
            let seen = recorder();
            // `/tmp` is `/private/tmp` on macOS, so the expectation is produced
            // by the same resolver the route uses rather than hardcoded.
            let expected = target("/tmp/work", "notes/demo.txt");
            let expected_str = expected.to_str().unwrap().to_owned();
            let wire = expected_str.clone();
            let server = MockHcServer::start(
                "file-io",
                2,
                &["hello", "file.read", "file.write"],
                seen.clone(),
                move |method, params| match method {
                    "file.write" => {
                        let encoded = params["data"].as_str().unwrap();
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .unwrap();
                        assert_eq!(decoded, b"hello from herdr-core\n");
                        // The wire carries the resolved absolute target and no
                        // `cwd`, so `hc`'s own cwd cannot change the file (#936).
                        assert_eq!(params["path"], wire);
                        assert!(params.get("cwd").is_none(), "{params}");
                        Ok(json!({"path": params["path"], "bytes": 23}))
                    }
                    "file.read" => {
                        assert_eq!(params["path"], wire);
                        assert!(params.get("cwd").is_none(), "{params}");
                        assert_eq!(params["offset"], 2);
                        assert_eq!(params["limit"], 3);
                        Ok(json!({
                            "text": "line two\nline three\n",
                            "first": 2,
                            "last": 3,
                            "lines": 9
                        }))
                    }
                    other => panic!("unexpected method {other}"),
                },
            );
            let client = server.client();

            let written = client
                .file_write(&expected, b"hello from herdr-core\n")
                .unwrap();
            assert_eq!(written["ok"], true);
            assert_eq!(written["runtime_backend"], "herdr-core");
            assert_eq!(written["path"], expected_str);
            assert!(expected.is_absolute(), "{}", expected.display());
            assert_eq!(written["bytes"], 23);

            let read = client.file_read(&expected, Some(2), Some(3)).unwrap();
            assert_eq!(read["ok"], true);
            assert_eq!(read["runtime_backend"], "herdr-core");
            assert_eq!(read["text"], "line two\nline three\n");
            assert_eq!(read["first"], 2);
            assert_eq!(read["last"], 3);
            assert_eq!(read["lines"], 9);

            // The wire payload really was base64, not raw text.
            let write_params = recorded(&seen, "file.write");
            assert_eq!(write_params.len(), 1);
            let sent = write_params[0]["data"].as_str().unwrap();
            assert_eq!(sent, "aGVsbG8gZnJvbSBoZXJkci1jb3JlCg==");
            server.finish();
        }

        #[test]
        fn secret_like_paths_are_refused_before_the_socket_is_contacted() {
            let client = HerdrCoreClient::new(absent_socket_path("secret"));
            for path in [
                ".env",
                ".env.production",
                "config/id_rsa",
                "certs/server.pem",
                "state/api-token.json",
            ] {
                let target = super::herdr_core_absolute_file_target(
                    path,
                    Some(Path::new("/tmp/herdr-core-secret-probe")),
                )
                .unwrap();
                let read = client.file_read(&target, None, None).unwrap_err();
                assert_eq!(read.code, "secret_path_denied", "file.read {path}");
                assert_eq!(read.kind.as_deref(), Some("secret_path_denied"));

                let write = client.file_write(&target, b"x").unwrap_err();
                assert_eq!(write.code, "secret_path_denied", "file.write {path}");

                let edit = client
                    .file_edit(&target, &[("a".to_owned(), "b".to_owned())])
                    .unwrap_err();
                assert_eq!(edit.code, "secret_path_denied", "file.edit {path}");
            }
            // Control: a non-secret path on the same absent socket is reachable and
            // fails at the transport, proving the refusals above were pre-socket.
            let reachable = client
                .file_read(
                    Path::new("/tmp/herdr-core-probe/notes/readme.md"),
                    None,
                    None,
                )
                .unwrap_err();
            assert_eq!(reachable.code, "herdr_core_unreachable");
        }

        #[test]
        fn relative_paths_and_directories_are_refused_before_any_file_access() {
            // #936: a relative `path`/`cwd` resolves against the herdr-core
            // process cwd, which is not this process's cwd, so neither the
            // secret-path guard nor the root lease could name the executed file.
            let client = HerdrCoreClient::new(absent_socket_path("ambiguous"));

            let no_cwd =
                super::herdr_core_absolute_file_target("notes/demo.txt", None).unwrap_err();
            assert_eq!(no_cwd.code, "herdr_core_path_ambiguous");
            assert_eq!(no_cwd.kind.as_deref(), Some("herdr_core_path_ambiguous"));

            let relative_cwd =
                super::herdr_core_absolute_directory("crates/herdr-mcp").unwrap_err();
            assert_eq!(relative_cwd.code, "herdr_core_path_ambiguous");

            // The primitives themselves refuse a relative target rather than
            // sending it to `hc`.
            for target in [Path::new("notes/demo.txt"), Path::new(".")] {
                assert_eq!(
                    client.file_read(target, None, None).unwrap_err().code,
                    "herdr_core_path_ambiguous",
                    "{}",
                    target.display()
                );
                assert_eq!(
                    client.file_write(target, b"x").unwrap_err().code,
                    "herdr_core_path_ambiguous"
                );
                assert_eq!(
                    client
                        .file_edit(target, &[("a".to_owned(), "b".to_owned())])
                        .unwrap_err()
                        .code,
                    "herdr_core_path_ambiguous"
                );
            }

            // An absolute directory still resolves normally (control).
            let resolved = super::herdr_core_absolute_directory("/tmp/work").unwrap();
            assert!(resolved.is_absolute(), "{}", resolved.display());
            assert!(resolved.ends_with("work"), "{}", resolved.display());
        }

        #[test]
        fn file_write_refuses_payloads_above_the_eight_mebibyte_bound() {
            let client = HerdrCoreClient::new(absent_socket_path("too-large"));
            let target = Path::new("/tmp/herdr-core-big/notes/big.txt");
            let oversized = vec![b'a'; HC_FILE_MAX_BYTES + 1];
            let refused = client.file_write(target, &oversized).unwrap_err();
            assert_eq!(refused.code, "file_too_large");
            assert_eq!(refused.kind.as_deref(), Some("file_too_large"));
            assert!(refused.message.contains("8388608"));

            // Exactly 8 MiB is admitted by the bound and then stops at the absent
            // socket, so the check is an exclusive ceiling rather than a guess.
            let at_bound = client
                .file_write(target, &vec![b'a'; HC_FILE_MAX_BYTES])
                .unwrap_err();
            assert_eq!(at_bound.code, "herdr_core_unreachable");
        }

        #[test]
        fn readonly_mode_refuses_mutations_before_contacting_the_socket() {
            // `HERDR_MCP_READONLY` is process-global and Rust tests share one
            // process, so the probe runs in a child of this same test binary where
            // the environment cannot leak into sibling tests.
            let executable = std::env::current_exe().unwrap();
            let output = std::process::Command::new(executable)
                .args(["readonly_child_probe", "--ignored", "--nocapture"])
                .env("HERDR_MCP_READONLY", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "readonly child probe failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        #[test]
        #[ignore = "child probe launched by readonly_mode_refuses_mutations_before_contacting_the_socket"]
        fn readonly_child_probe() {
            assert_eq!(std::env::var("HERDR_MCP_READONLY").as_deref(), Ok("1"));
            let client = HerdrCoreClient::new(absent_socket_path("readonly-child"));

            assert_eq!(
                client
                    .file_write(Path::new("/tmp/work/notes/demo.txt"), b"x")
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            assert_eq!(
                client
                    .file_edit(
                        Path::new("/tmp/work/notes/demo.txt"),
                        &[("a".to_owned(), "b".to_owned())]
                    )
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            assert_eq!(
                client
                    .terminal_run(None, Some("true"), None, None, None)
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            assert_eq!(
                client.terminal_kill("t9").unwrap_err().code,
                "readonly_mode"
            );
            // P2-5: the guard lives in the shared `machine.exec` primitive, so
            // the `argv` and shell entrypoints refuse on their own.
            assert_eq!(
                client
                    .machine_exec(&["true".to_owned()], None, Some(1))
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            assert_eq!(
                client
                    .machine_exec_line("true", None, Some(1))
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            // Reads and waits stay available in readonly mode: they only fail at the
            // transport, never with `readonly_mode`.
            assert_eq!(
                client
                    .file_read(Path::new("/tmp/work/notes/demo.txt"), None, None)
                    .unwrap_err()
                    .code,
                "herdr_core_unreachable"
            );
            assert_eq!(
                client
                    .terminal_wait(Some("t9"), None, Some(5.0))
                    .unwrap_err()
                    .code,
                "herdr_core_unreachable"
            );
        }

        #[test]
        fn file_edit_sends_exact_once_edits_and_maps_edit_failed() {
            let seen = recorder();
            let expected = target("/tmp/work", "src/lib.rs");
            let expected_str = expected.to_str().unwrap().to_owned();
            let expected_wire = expected_str.clone();
            let call = Arc::new(AtomicU64::new(0));
            let server = MockHcServer::start(
                "file-edit",
                2,
                &["hello", "file.edit"],
                seen.clone(),
                move |method, params| {
                    assert_eq!(method, "file.edit");
                    assert_eq!(params["path"], expected_wire);
                    assert!(params.get("cwd").is_none(), "{params}");
                    assert_eq!(
                        params["edits"],
                        json!([
                            {"old": "fn a() {}", "new": "fn a() { 1 }"},
                            {"old": "fn b() {}", "new": "fn b() { 2 }"}
                        ])
                    );
                    if call.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(json!({"path": params["path"], "bytes": 42}))
                    } else {
                        Err(json!({
                            "code": -32000,
                            "message": "'fn a() {}' is not unique",
                            "data": { "kind": "edit_failed" }
                        }))
                    }
                },
            );
            let client = server.client();
            let edits = vec![
                ("fn a() {}".to_owned(), "fn a() { 1 }".to_owned()),
                ("fn b() {}".to_owned(), "fn b() { 2 }".to_owned()),
            ];

            let applied = client.file_edit(&expected, &edits).unwrap();
            assert_eq!(applied["ok"], true);
            assert_eq!(applied["path"], expected_str);
            assert_eq!(applied["bytes"], 42);
            assert_eq!(applied["edits_applied"], 2);

            let failure = client.file_edit(&expected, &edits).unwrap_err();
            assert_eq!(failure.code, "herdr_core_edit_failed");
            assert_eq!(failure.kind.as_deref(), Some("edit_failed"));

            // Both edits travelled in one all-or-none request, never one-by-one.
            assert_eq!(recorded(&seen, "file.edit").len(), 2);
            server.finish();
        }

        #[test]
        fn terminal_run_read_kill_and_wait_map_typed_results() {
            let seen = recorder();
            let server = MockHcServer::start(
                "terminal",
                4,
                &[
                    "hello",
                    "terminal.run",
                    "terminal.read",
                    "terminal.kill",
                    "terminal.wait",
                ],
                seen.clone(),
                |method, params| match method {
                    "terminal.run" => {
                        assert_eq!(params["argv"], json!(["sleep", "30"]));
                        assert!(params.get("line").is_none());
                        assert_eq!(params["cwd"], "/tmp/work");
                        assert_eq!(params["name"], "worker-1");
                        assert_eq!(params["labels"], json!({"role": "worker"}));
                        Ok(json!({
                            "address": "t9",
                            "state": "live",
                            "cwd": "/tmp/work",
                            "argv": ["sleep", "30"],
                            "labels": {"role": "worker"},
                            "name": "worker-1",
                            "parent": "t1",
                            "turn": 0,
                            "created_ms": 1700000000000_u64,
                            "screen": "exact",
                            "cols": 200,
                            "rows": 120
                        }))
                    }
                    "terminal.read" => {
                        assert_eq!(params["terminal"], "t9");
                        assert_eq!(params["lines"], 40);
                        Ok(json!({
                            "text": "working...\n",
                            "complete": false,
                            "how": "screen",
                            "why": "working"
                        }))
                    }
                    "terminal.kill" => {
                        assert_eq!(params["terminal"], "t9");
                        Ok(json!({
                            "address": "t9",
                            "state": "finished",
                            "reason": "killed",
                            "exit_code": null,
                            "signal": 15,
                            "labels": {},
                            "argv": ["sleep", "30"],
                            "cwd": "/tmp/work",
                            "created_ms": 1700000000000_u64,
                            "screen": "exact",
                            "cols": 200,
                            "rows": 120
                        }))
                    }
                    "terminal.wait" => {
                        assert_eq!(params["terminal"], "t9");
                        assert_eq!(params["output"], "READY");
                        // A caller asking for 10 minutes still gets at most 20 s.
                        assert_eq!(params["timeout"], HC_TERMINAL_WAIT_MAX_SECS);
                        Ok(json!({
                            "terminal": "t9",
                            "ended": false,
                            "reason": "timeout",
                            "status": "working",
                            "turn": 3,
                            "children": []
                        }))
                    }
                    other => panic!("unexpected method {other}"),
                },
            );
            let client = server.client();

            let mut labels = BTreeMap::new();
            labels.insert("role".to_owned(), "worker".to_owned());
            let argv = vec!["sleep".to_owned(), "30".to_owned()];
            let started = client
                .terminal_run(
                    Some(&argv),
                    None,
                    Some(Path::new("/tmp/work")),
                    Some("worker-1"),
                    Some(&labels),
                )
                .unwrap();
            assert_eq!(started["ok"], true);
            assert_eq!(started["terminal"], "t9");
            assert_eq!(started["state"], "live");
            assert_eq!(started["name"], "worker-1");
            assert_eq!(started["labels"], json!({"role": "worker"}));

            let read = client.terminal_read("t9", Some(40)).unwrap();
            assert_eq!(read["ok"], true);
            assert_eq!(read["text"], "working...\n");
            assert_eq!(read["complete"], false);
            assert_eq!(read["how"], "screen");
            assert_eq!(read["why"], "working");

            let killed = client.terminal_kill("t9").unwrap();
            assert_eq!(killed["ok"], true);
            assert_eq!(killed["state"], "finished");
            assert_eq!(killed["reason"], "killed");
            assert_eq!(killed["signal"], 15);

            let waited = client
                .terminal_wait(Some("t9"), Some("READY"), Some(600.0))
                .unwrap();
            assert_eq!(
                waited,
                WaitResult {
                    ended: false,
                    reason: "timeout".to_owned(),
                    status: Some("working".to_owned()),
                    exit_code: None,
                    signal: None,
                    turn: 3,
                }
            );
            assert_eq!(waited.as_json()["reason"], "timeout");
            // Each terminal method went to its own RPC, never a shared fallback.
            for method in [
                "terminal.run",
                "terminal.read",
                "terminal.kill",
                "terminal.wait",
            ] {
                assert_eq!(recorded(&seen, method).len(), 1, "{method}");
            }
            server.finish();
        }

        /// #936: `herdr_exec_start`/`read`/`kill` have no safe `herdr-core`
        /// semantics — `hc` terminal addresses are one namespace shared with
        /// unrelated agent terminals, with no runtime-owned session id and no
        /// byte cursor — so they fail closed without ever contacting `hc`.
        /// The socket does not exist here, so a route that still created, read
        /// or killed a terminal would report `herdr_core_unreachable` instead.
        #[test]
        fn exec_session_tools_fail_closed_without_touching_hc() {
            let client = HerdrCoreClient::new(absent_socket_path("exec-session"));
            for (operation, value) in [
                (
                    "herdr_exec_start",
                    super::herdr_core_exec_start(
                        &client,
                        &json!({"root": "/tmp/work", "command": "printf orphan"}),
                    ),
                ),
                (
                    "herdr_exec_read",
                    super::herdr_core_exec_read(&client, &json!({"session_id": "t1"})),
                ),
                (
                    "herdr_exec_kill",
                    super::herdr_core_exec_kill(&client, &json!({"session_id": "t1"})),
                ),
            ] {
                assert_eq!(value["ok"], false, "{operation}: {value}");
                assert_eq!(
                    value["code"], "runtime_capability_unsupported",
                    "{operation}: {value}"
                );
                assert_eq!(
                    value["required_capability"], "exec_session_ownership",
                    "{operation}: {value}"
                );
                assert_eq!(value["delivery_state"], "not_delivered", "{operation}");
                assert_eq!(value["runtime_backend"], "herdr-core", "{operation}");
                assert_eq!(value["fallback_allowed"], false, "{operation}");
            }

            // The raw read channel refuses the same unowned terminal addresses.
            for method in ["terminal.read", "terminal.kill", "terminal.run"] {
                let refused = client
                    .call_allowed_method(method, json!({"terminal": "t1"}))
                    .unwrap_err();
                assert_eq!(refused.code, "herdr_core_method_prohibited", "{method}");
            }
            // Shared terminal metadata has no ownership check either.
            assert!(super::hc_forwarding_refusal("terminal.list").is_some());
            assert!(super::hc_forwarding_refusal("terminal.get").is_some());
        }

        #[test]
        fn unadvertised_primitives_fail_closed_before_the_rpc() {
            // The mock refuses any non-hello method with a panic, so reaching the
            // socket would fail loudly instead of silently downgrading.
            let server =
                MockHcServer::start("no-file", 1, &["hello"], recorder(), |method, _params| {
                    panic!("unexpected method {method}")
                });
            let client = server.client();
            let refused = client
                .file_read(
                    Path::new("/tmp/herdr-core-probe/notes/readme.md"),
                    None,
                    None,
                )
                .unwrap_err();
            assert_eq!(refused.code, "runtime_capability_unsupported");
            server.finish();
        }

        #[test]
        fn exec_steps_are_rejected_before_command_mode_is_selected() {
            for operation in ["herdr_exec", "herdr_exec_start"] {
                for steps in [json!([]), json!([{"command": "echo second"}]), json!(false)] {
                    for invocation in [
                        json!({"command": "echo first"}),
                        json!({"program": "echo", "args": ["first"]}),
                    ] {
                        let mut args = invocation;
                        args["steps"] = steps.clone();
                        let error = hc_exec_invocation(&args, operation).unwrap_err();
                        assert_eq!(error["required_capability"], "exec_steps", "{args}");
                        assert_eq!(error["operation"], operation);
                        assert_eq!(error["delivery_state"], "not_delivered");
                    }
                }
                for args in [
                    json!({"command": "echo first"}),
                    json!({"command": "echo first", "steps": null}),
                ] {
                    assert!(matches!(
                        hc_exec_invocation(&args, operation),
                        Ok(HcExecInvocation::Line(_))
                    ));
                }
            }
        }

        #[test]
        fn invalid_primitive_arguments_are_refused_before_the_socket() {
            let client = HerdrCoreClient::new(absent_socket_path("invalid-args"));
            let probe = Path::new("/tmp/herdr-core-probe/notes/readme.md");
            assert_eq!(
                client.file_read(probe, Some(0), None).unwrap_err().code,
                "invalid_params"
            );
            assert_eq!(
                client.file_write(probe, b"x").unwrap_err().code,
                "herdr_core_unreachable"
            );
            assert_eq!(
                client.file_edit(probe, &[]).unwrap_err().code,
                "invalid_params"
            );
            assert_eq!(
                client
                    .terminal_run(Some(&[]), None, None, None, None)
                    .unwrap_err()
                    .code,
                "invalid_params"
            );
            assert_eq!(
                client
                    .terminal_run(
                        Some(&["sleep".to_owned()]),
                        Some("sleep 1"),
                        None,
                        None,
                        None
                    )
                    .unwrap_err()
                    .code,
                "invalid_params"
            );
            assert_eq!(
                client
                    .terminal_run(None, Some("sleep 1"), None, Some("Bad Name"), None)
                    .unwrap_err()
                    .code,
                "invalid_params"
            );
            assert_eq!(
                client.terminal_read("", None).unwrap_err().code,
                "invalid_params"
            );
            assert_eq!(
                client.terminal_kill("  ").unwrap_err().code,
                "invalid_params"
            );
            for timeout in [0.0_f64, -1.0, f64::NAN, f64::INFINITY] {
                assert_eq!(
                    client
                        .terminal_wait(Some("t9"), None, Some(timeout))
                        .unwrap_err()
                        .code,
                    "invalid_params",
                    "timeout {timeout}"
                );
            }
            assert_eq!(
                client
                    .terminal_wait(Some("t9"), Some(""), Some(1.0))
                    .unwrap_err()
                    .code,
                "invalid_params"
            );
        }

        fn unique_tmp_dir(tag: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "herdr-core-paths-{tag}-{}-{}",
                std::process::id(),
                NEXT_MOCK.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn dangling_symlink_chains_fail_closed_and_parent_follows_symlink() {
            // Canonicalize the fixture root so platform temp-directory aliases
            // do not consume the symlink budget intended for this chain.
            let root = std::fs::canonicalize(unique_tmp_dir("symlink-chain")).unwrap();
            let limit = super::HC_SECRET_PATH_SYMLINK_HOPS;
            for index in 0..=limit {
                let destination = if index == limit {
                    ".env".to_owned()
                } else {
                    format!("link-{}", index + 1)
                };
                std::os::unix::fs::symlink(destination, root.join(format!("link-{index}")))
                    .unwrap();
            }
            let client = HerdrCoreClient::new(absent_socket_path("symlink-chain"));
            let too_long = root.join("link-0");
            assert_eq!(
                client.file_read(&too_long, None, None).unwrap_err().code,
                "herdr_core_path_ambiguous"
            );
            assert_eq!(
                client.file_write(&too_long, b"x").unwrap_err().code,
                "herdr_core_path_ambiguous"
            );
            assert_eq!(
                client
                    .file_edit(&too_long, &[("a".to_owned(), "b".to_owned())])
                    .unwrap_err()
                    .code,
                "herdr_core_path_ambiguous"
            );
            assert_eq!(
                client
                    .file_read(&root.join("link-1"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );

            std::fs::create_dir_all(root.join("safe/nested")).unwrap();
            std::fs::create_dir_all(root.join(".ssh/nested")).unwrap();
            std::os::unix::fs::symlink("../.ssh/nested", root.join("safe/alias")).unwrap();
            let path = root.join("safe/alias/../config");
            let resolved =
                super::herdr_core_absolute_file_target(path.to_str().unwrap(), None).unwrap();
            assert_eq!(
                resolved,
                std::fs::canonicalize(root.join(".ssh"))
                    .unwrap()
                    .join("config")
            );
            assert_eq!(
                client.file_read(&path, None, None).unwrap_err().code,
                "secret_path_denied"
            );
            let missing_parent = root.join("missing/../readme");
            assert_eq!(
                super::herdr_core_absolute_file_target(missing_parent.to_str().unwrap(), None)
                    .unwrap_err()
                    .code,
                "herdr_core_path_ambiguous"
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn secret_paths_are_refused_through_symlinks_and_secret_working_directories() {
            let root = unique_tmp_dir("symlinks");
            let root_str = root.to_str().unwrap();
            let env_file = root.join(".env");
            std::fs::write(&env_file, "TOKEN=1\n").unwrap();
            // A harmless name pointing at a real secret file.
            std::os::unix::fs::symlink(&env_file, root.join("notes.txt")).unwrap();
            // A harmless name pointing at a secret file that does not exist yet.
            std::os::unix::fs::symlink("state/.env", root.join("later.txt")).unwrap();
            let ssh_dir = root.join(".ssh");
            std::fs::create_dir_all(&ssh_dir).unwrap();
            std::fs::write(ssh_dir.join("config"), "Host *\n").unwrap();
            std::fs::write(root.join("readme.md"), "ok\n").unwrap();
            std::fs::create_dir(root.join("sub")).unwrap();

            let client = HerdrCoreClient::new(absent_socket_path("symlinks"));
            let ssh_str = ssh_dir.to_str().unwrap();

            // `notes.txt -> .env` resolved from an absolute cwd, and spelled as
            // an absolute path.
            assert_eq!(
                client
                    .file_read(&target(root_str, "notes.txt"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            assert_eq!(
                client
                    .file_read(&root.join("notes.txt"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Dangling symlink to a not-yet-created `.env`.
            assert_eq!(
                client
                    .file_read(&target(root_str, "later.txt"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // The secret hides in `cwd` while the leaf name looks clean.
            assert_eq!(
                client
                    .file_read(&target(ssh_str, "config"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // `..` still resolves onto the refused target.
            assert_eq!(
                client
                    .file_read(&target(root_str, "sub/../.env"), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Mutations run the same guard.
            assert_eq!(
                client
                    .file_write(&target(root_str, "notes.txt"), b"x")
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            assert_eq!(
                client
                    .file_edit(
                        &target(root_str, "notes.txt"),
                        &[("a".to_owned(), "b".to_owned())]
                    )
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Control: a benign absolute target is not refused, so the guard did
            // not start blanket-rejecting resolved paths; it stops at the transport.
            assert_eq!(
                client
                    .file_read(&target(root_str, "readme.md"), None, None)
                    .unwrap_err()
                    .code,
                "herdr_core_unreachable"
            );
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn edit_text_above_the_eight_mebibyte_bound_is_refused() {
            let client = HerdrCoreClient::new(absent_socket_path("edit-too-large"));
            let edits = vec![("a".repeat(HC_FILE_MAX_BYTES), "b".to_owned())];
            let refused = client
                .file_edit(Path::new("/tmp/work/notes/demo.txt"), &edits)
                .unwrap_err();
            assert_eq!(refused.code, "edit_too_large");
            assert_eq!(refused.kind.as_deref(), Some("edit_too_large"));
        }

        #[test]
        fn terminal_wait_and_machine_exec_get_a_budget_that_outlives_the_handshake_timeout() {
            let seen = recorder();
            let server = MockHcServer::start(
                "budget",
                3,
                &["hello", "terminal.wait", "machine.exec", "file.read"],
                seen.clone(),
                |method, _params| {
                    // Longer than the client's 150 ms session budget: only a call
                    // whose budget is derived from its own timeout can survive.
                    thread::sleep(Duration::from_millis(500));
                    match method {
                        "terminal.wait" => Ok(json!({
                            "terminal": "t9",
                            "ended": false,
                            "reason": "timeout",
                            "status": "working",
                            "turn": 1,
                            "children": []
                        })),
                        "machine.exec" => Ok(json!({
                            "code": 0,
                            "stdout": "",
                            "stderr": "",
                            "timed_out": false,
                            "truncated": false,
                            "ms": 500
                        })),
                        "file.read" => Ok(json!({
                            "text": "late\n",
                            "first": 1,
                            "last": 1,
                            "lines": 1
                        })),
                        other => panic!("unexpected method {other}"),
                    }
                },
            );
            let client =
                HerdrCoreClient::with_timeout(server.socket_path(), Duration::from_millis(150));

            let waited = client.terminal_wait(Some("t9"), None, Some(5.0)).unwrap();
            assert_eq!(waited.reason, "timeout");

            let exec = client
                .machine_exec(&["true".to_owned()], None, Some(1))
                .unwrap();
            assert_eq!(exec["ok"], true);

            // Control: a plain call still stops at the 150 ms session budget.
            let late = client
                .file_read(Path::new("/tmp/work/notes/readme.md"), None, None)
                .unwrap_err();
            assert_eq!(late.code, "timeout");

            // Both bounded calls carried their own explicit server-side timeout.
            assert_eq!(recorded(&seen, "terminal.wait")[0]["timeout"], 5.0);
            assert_eq!(recorded(&seen, "machine.exec")[0]["timeout"], 1);
            server.finish();
        }

        #[test]
        fn terminal_wait_sends_the_bounded_default_when_the_caller_omits_it() {
            let seen = recorder();
            let server = MockHcServer::start(
                "wait-default",
                1,
                &["hello", "terminal.wait"],
                seen.clone(),
                |_method, _params| {
                    Ok(json!({
                        "terminal": "t9",
                        "ended": false,
                        "reason": "timeout",
                        "status": "working",
                        "turn": 4,
                        "children": []
                    }))
                },
            );
            let waited = server
                .client()
                .terminal_wait(Some("t9"), None, None)
                .unwrap();
            assert_eq!(waited.reason, "timeout");
            assert_eq!(
                recorded(&seen, "terminal.wait")[0]["timeout"],
                HC_TERMINAL_WAIT_MAX_SECS
            );
            server.finish();
        }

        #[test]
        fn file_read_reassembles_a_reply_larger_than_one_read_chunk() {
            let body = "x".repeat(32 * 1024);
            let expected = body.clone();
            let server = MockHcServer::start(
                "chunked",
                1,
                &["hello", "file.read"],
                recorder(),
                move |method, _params| {
                    assert_eq!(method, "file.read");
                    Ok(json!({
                        "text": body,
                        "first": 1,
                        "last": 1,
                        "lines": 1
                    }))
                },
            );
            let read = server
                .client()
                .file_read(Path::new("/tmp/work/notes/big.txt"), None, None)
                .unwrap();
            assert_eq!(read["text"], expected);
            assert_eq!(read["text"].as_str().unwrap().len(), 32 * 1024);
            server.finish();
        }

        #[test]
        fn crossed_reply_ids_fail_closed() {
            let server = MockHcServer::start_with(
                MockHcOptions {
                    reply_id_offset: 1000,
                    ..MockHcOptions::default()
                },
                "id-mismatch",
                1,
                &["hello"],
                recorder(),
                |_method, _params| Ok(json!({})),
            );
            let refused = server.client().handshake().unwrap_err();
            assert_eq!(refused.code, "herdr_core_response_id_mismatch");
            assert_eq!(refused.kind.as_deref(), Some("response_id_mismatch"));
            server.finish();
        }

        #[test]
        fn framed_hello_fails_closed() {
            let server = MockHcServer::start_with(
                MockHcOptions {
                    framed: true,
                    ..MockHcOptions::default()
                },
                "framed",
                1,
                &["hello"],
                recorder(),
                |_method, _params| Ok(json!({})),
            );
            let refused = server.client().handshake().unwrap_err();
            assert_eq!(refused.code, "herdr_core_framed_unsupported");
            server.finish();
        }
    }
}
