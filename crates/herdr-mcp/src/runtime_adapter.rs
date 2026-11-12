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
    pub hc_session: Option<String>,
    pub xdg_state_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl EnvOverrides {
    pub fn from_process() -> Self {
        Self {
            runtime_backend: std::env::var(RUNTIME_BACKEND_ENV).ok(),
            hc_socket: std::env::var_os("HC_SOCKET").map(PathBuf::from),
            hc_session: std::env::var("HC_SESSION").ok(),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
        }
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
    resolve_backend_selection_with_env(Some(config), explicit, &EnvOverrides::from_process())
}

pub fn resolve_process_backend_selection() -> Result<RuntimeBackendSelection, String> {
    let env = EnvOverrides::from_process();
    if let Ok(paths) = RuntimePaths::discover()
        && paths.config_file.exists()
    {
        let cfg = Config::load(&paths.config_file)?;
        let explicit =
            cfg.runtime_backend != RuntimeBackend::Tui || cfg.runtime_herdr_core_socket.is_some();
        return resolve_backend_selection_with_env(Some(&cfg), explicit, &env);
    }
    resolve_backend_selection_with_env(None, false, &env)
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
            if !capabilities.server_status || !hello.methods.contains("terminal.list") {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message:
                        "herdr-core did not advertise server.status + terminal.list in hello.methods"
                            .to_owned(),
                });
            }
            let status = rpc_call_line(stream, &self.socket_path, "server.status", json!({}), call_timeout)?;
            let list = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.list",
                json!({"all": false}),
                call_timeout,
            )?;
            let terminals_raw = list
                .get("terminals")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let terminals: Vec<Value> = terminals_raw
                .into_iter()
                .map(|term| project_herdr_core_terminal(&term))
                .collect();
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
                    "capabilities": capabilities.as_json(),
                },
                "terminals": terminals,
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
            let mut params = Map::new();
            params.insert("argv".to_owned(), json!(argv));
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

    /// `file.read`: same secret-path denylist as `fs_security`, refused before
    /// the socket is contacted (fail-closed, no partial disclosure).
    #[allow(dead_code)]
    pub fn file_read(
        &self,
        path: &str,
        cwd: Option<&Path>,
        offset_1based: Option<u64>,
        limit_lines: Option<u64>,
    ) -> Result<Value, HerdrCoreError> {
        let path = hc_required_path(path, "file.read")?;
        hc_secret_like_path_guard(path, cwd)?;
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
            hc_insert_cwd(&mut params, cwd);
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
    /// run before the socket is contacted; `data` is base64 per the OpenRPC schema.
    #[allow(dead_code)]
    pub fn file_write(
        &self,
        path: &str,
        cwd: Option<&Path>,
        data: &[u8],
    ) -> Result<Value, HerdrCoreError> {
        let path = hc_required_path(path, "file.write")?;
        hc_mutation_guard("herdr_fs_write")?;
        hc_secret_like_path_guard(path, cwd)?;
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
            hc_insert_cwd(&mut params, cwd);
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
                "path": raw.get("path").and_then(Value::as_str).unwrap_or(path),
                "bytes": raw.get("bytes").and_then(Value::as_u64).unwrap_or(data.len() as u64),
            }))
        })
    }

    /// `file.edit`: every `old` must match exactly once in the file as it was and
    /// `hc` applies all edits or none, so a failed call is never partially applied.
    #[allow(dead_code)]
    pub fn file_edit(
        &self,
        path: &str,
        cwd: Option<&Path>,
        edits: &[(String, String)],
    ) -> Result<Value, HerdrCoreError> {
        let path = hc_required_path(path, "file.edit")?;
        if edits.is_empty() {
            return Err(hc_invalid_params(
                "file.edit requires at least one (old, new) replacement",
            ));
        }
        hc_mutation_guard("herdr_fs_edit")?;
        hc_secret_like_path_guard(path, cwd)?;
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
            hc_insert_cwd(&mut params, cwd);
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
                "path": raw.get("path").and_then(Value::as_str).unwrap_or(path),
                "bytes": raw.get("bytes").and_then(Value::as_u64),
                "edits_applied": edits.len(),
            }))
        })
    }

    /// `terminal.run`: `argv` or `line` (never both), optional `cwd`/`name`/`labels`.
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
        mut params: Value,
    ) -> Result<Value, HerdrCoreError> {
        let method = method.trim();
        if PROHIBITED_HC_METHODS.contains(&method) {
            return Err(HerdrCoreError {
                code: "herdr_core_method_prohibited".to_owned(),
                kind: None,
                message: format!(
                    "herdr-core method '{method}' is prohibited by herdr-mcp runtime ownership rules"
                ),
            });
        }
        if method == "terminal.send"
            && let Some(obj) = params.as_object_mut()
        {
            if obj.get("force").and_then(Value::as_bool) == Some(true) {
                return Err(HerdrCoreError {
                    code: "human_boundary_prohibited".to_owned(),
                    kind: None,
                    message: "terminal.send force=true is prohibited (human boundary)".to_owned(),
                });
            }
            obj.insert("force".to_owned(), json!(false));
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
                "calls_you": false,
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
/// Fail-closed secret-path refusal for a `cwd` + `path` pair, resolved the way
/// `hc` will resolve it.
///
/// Checking the caller's raw string is not enough: `notes.txt -> .env` hides the
/// secret behind a harmless name, and `cwd=/work/.ssh` + `path=config` hides it in
/// the working directory. So the guard checks (1) the lexically normalized
/// combination of `cwd` and a relative `path`, and (2) the same path after
/// following symlinks on disk, where a path that does not exist yet is resolved
/// through its deepest existing ancestor plus the remaining leaf components.
fn hc_secret_like_path_guard(path: &str, cwd: Option<&Path>) -> Result<(), HerdrCoreError> {
    let raw = Path::new(path);
    let candidate = match cwd {
        Some(cwd) if !raw.is_absolute() => cwd.join(raw),
        _ => raw.to_path_buf(),
    };
    let lexical = hc_lexical_normalize(&candidate);
    let resolved = hc_symlink_resolved_path(&lexical, HC_SECRET_PATH_SYMLINK_HOPS);
    for checked in [&lexical, &resolved] {
        if hc_secret_like_path(checked) {
            return Err(HerdrCoreError {
                code: "secret_path_denied".to_owned(),
                kind: Some("secret_path_denied".to_owned()),
                message: format!(
                    "refusing secret-like path '{path}' (resolved '{}'): herdr-core file primitives apply the fs_security denylist after cwd join, lexical normalization and symlink resolution",
                    checked.display()
                ),
            });
        }
    }
    Ok(())
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

/// Collapse `.` and `..` so `sub/../.env` is judged as `.env`.
fn hc_lexical_normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // Popping the root is a no-op, so absolute paths cannot escape `/`.
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    if output.as_os_str().is_empty() {
        output.push(".");
    }
    output
}

/// Resolve symlinks on disk without requiring the final target to exist.
///
/// A dangling symlink counts as existing, so a link that points at a `.env` file
/// nobody has created yet is still resolved and checked. Components that do not
/// exist are kept and appended to the resolved ancestor.
fn hc_symlink_resolved_path(path: &Path, hops: usize) -> PathBuf {
    if hops == 0 {
        return path.to_path_buf();
    }
    let mut probe = path;
    let mut missing_leafs: Vec<std::ffi::OsString> = Vec::new();
    while probe.symlink_metadata().is_err() {
        let Some(parent) = probe.parent() else {
            return path.to_path_buf();
        };
        if parent == probe {
            return path.to_path_buf();
        }
        if let Some(name) = probe.file_name() {
            missing_leafs.push(name.to_owned());
        }
        probe = parent;
    }
    let resolved = match std::fs::canonicalize(probe) {
        Ok(real) => real,
        Err(_) => match std::fs::read_link(probe) {
            Ok(target) => {
                let joined = if target.is_absolute() {
                    target
                } else {
                    probe
                        .parent()
                        .map(|parent| parent.join(&target))
                        .unwrap_or(target)
                };
                hc_symlink_resolved_path(&hc_lexical_normalize(&joined), hops - 1)
            }
            Err(_) => probe.to_path_buf(),
        },
    };
    let mut output = resolved;
    for name in missing_leafs.iter().rev() {
        output.push(name);
    }
    hc_lexical_normalize(&output)
}

fn hc_required_path<'a>(path: &'a str, method: &str) -> Result<&'a str, HerdrCoreError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(hc_invalid_params(format!("{method} path is required")));
    }
    Ok(path)
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

fn project_herdr_core_terminal(term: &Value) -> Value {
    let reported = term
        .get("reported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // OSC 7501 rule (#767 §3.1): never guess status from screen text; expose
    // `osc7501_status` only when the program actually reported via 7501.
    let osc7501_status = if reported {
        term.get("status").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    json!({
        "address": term.get("address").cloned().unwrap_or(Value::Null),
        "state": term.get("state").cloned().unwrap_or(Value::Null),
        "cwd": term.get("cwd").cloned().unwrap_or(Value::Null),
        "argv": term.get("argv").cloned().unwrap_or_else(|| json!([])),
        "labels": term.get("labels").cloned().unwrap_or_else(|| json!({})),
        "parent": term.get("parent").cloned().unwrap_or(Value::Null),
        "turn": term.get("turn").cloned().unwrap_or(Value::Null),
        "reported": reported,
        "osc7501_status": osc7501_status,
    })
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

/// Fail-closed admission for one frozen Epoch-5 public tool invocation (#922).
///
/// The tool catalog itself never changes: this gate decides only whether the
/// *selected backend* may serve an already-declared tool. `tui` is the default
/// and admits everything without touching a socket; `herdr-core` re-negotiates
/// `hello` and applies the adapter's capability table. Any backend-selection
/// error (for example `HERDR_MCP_RUNTIME_BACKEND=auto`) fails closed instead of
/// falling back to the TUI path.
pub fn public_tool_admission(tool_name: &str) -> Result<(), Value> {
    let selection = resolve_process_backend_selection().map_err(|error| {
        invalid_runtime_backend_tool_error(tool_name, &error, "backend_selection")
    })?;
    match selection.backend {
        RuntimeBackend::Tui => Ok(()),
        RuntimeBackend::HerdrCore => {
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
            Ok(())
        }
    }
}

/// Public tools whose only implementation is the TUI snapshot/socket path.
const TUI_ROUTED_PUBLIC_TOOLS: &[&str] = &[
    "herdr_fs_read",
    "herdr_fs_list",
    "herdr_fs_grep",
    "herdr_fs_image",
    "herdr_fs_write",
    "herdr_fs_edit",
    "herdr_fs_patch",
    "herdr_git",
    "herdr_exec",
    "herdr_exec_start",
    "herdr_exec_read",
    "herdr_exec_kill",
    "herdr_prompt",
    "herdr_since",
];

/// Admission for the `herdr_call` tail that forwards an unhandled method to the
/// live Herdr TUI socket. Local `herdr_mcp.*` methods stay available; any
/// non-TUI backend must not reach the TUI forwarding path (#922).
pub fn forwarded_method_admission(method: &str) -> Result<(), Value> {
    let selection = resolve_process_backend_selection()
        .map_err(|error| invalid_runtime_backend_tool_error(method, &error, "backend_selection"))?;
    if selection.backend == RuntimeBackend::Tui || method.starts_with("herdr_mcp.") {
        return Ok(());
    }
    Err(json!({
        "ok": false,
        "code": "runtime_capability_unsupported",
        "operation": method,
        "required_capability": "tui_runtime_forward",
        "runtime_backend": selection.backend.as_str(),
        "fallback_allowed": false,
        "retryable": false,
        "delivery_state": "not_delivered",
        "failure_phase": "backend_capability",
        "message": format!(
            "'{method}' is forwarded to the live Herdr TUI socket, which the selected {} backend does not provide; no TUI fallback is allowed",
            selection.backend.as_str()
        ),
    }))
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
            hc_session: Some("uat768".to_owned()),
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
                hc_session: Some(rejected_session.to_owned()),
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
        assert_eq!(inspected["terminals"][0]["osc7501_status"]["state"], "idle");
        // Unreported terminal never exposes guessed status (#767 §3.1)
        assert_eq!(inspected["terminals"][1]["osc7501_status"], Value::Null);

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

        #[test]
        fn file_write_encodes_base64_and_file_read_maps_line_window() {
            let seen = recorder();
            let server = MockHcServer::start(
                "file-io",
                2,
                &["hello", "file.read", "file.write"],
                seen.clone(),
                |method, params| match method {
                    "file.write" => {
                        let encoded = params["data"].as_str().unwrap();
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .unwrap();
                        assert_eq!(decoded, b"hello from herdr-core\n");
                        assert_eq!(params["path"], "notes/demo.txt");
                        assert_eq!(params["cwd"], "/tmp/work");
                        Ok(json!({"path": "/tmp/work/notes/demo.txt", "bytes": 23}))
                    }
                    "file.read" => {
                        assert_eq!(params["path"], "notes/demo.txt");
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
                .file_write(
                    "notes/demo.txt",
                    Some(Path::new("/tmp/work")),
                    b"hello from herdr-core\n",
                )
                .unwrap();
            assert_eq!(written["ok"], true);
            assert_eq!(written["runtime_backend"], "herdr-core");
            assert_eq!(written["path"], "/tmp/work/notes/demo.txt");
            assert_eq!(written["bytes"], 23);

            let read = client
                .file_read(
                    "notes/demo.txt",
                    Some(Path::new("/tmp/work")),
                    Some(2),
                    Some(3),
                )
                .unwrap();
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
                let read = client.file_read(path, None, None, None).unwrap_err();
                assert_eq!(read.code, "secret_path_denied", "file.read {path}");
                assert_eq!(read.kind.as_deref(), Some("secret_path_denied"));

                let write = client.file_write(path, None, b"x").unwrap_err();
                assert_eq!(write.code, "secret_path_denied", "file.write {path}");

                let edit = client
                    .file_edit(path, None, &[("a".to_owned(), "b".to_owned())])
                    .unwrap_err();
                assert_eq!(edit.code, "secret_path_denied", "file.edit {path}");
            }
            // Control: a non-secret path on the same absent socket is reachable and
            // fails at the transport, proving the refusals above were pre-socket.
            let reachable = client
                .file_read("notes/readme.md", None, None, None)
                .unwrap_err();
            assert_eq!(reachable.code, "herdr_core_unreachable");
        }

        #[test]
        fn file_write_refuses_payloads_above_the_eight_mebibyte_bound() {
            let client = HerdrCoreClient::new(absent_socket_path("too-large"));
            let oversized = vec![b'a'; HC_FILE_MAX_BYTES + 1];
            let refused = client
                .file_write("notes/big.txt", None, &oversized)
                .unwrap_err();
            assert_eq!(refused.code, "file_too_large");
            assert_eq!(refused.kind.as_deref(), Some("file_too_large"));
            assert!(refused.message.contains("8388608"));

            // Exactly 8 MiB is admitted by the bound and then stops at the absent
            // socket, so the check is an exclusive ceiling rather than a guess.
            let at_bound = client
                .file_write("notes/big.txt", None, &vec![b'a'; HC_FILE_MAX_BYTES])
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
                    .file_write("notes/demo.txt", None, b"x")
                    .unwrap_err()
                    .code,
                "readonly_mode"
            );
            assert_eq!(
                client
                    .file_edit("notes/demo.txt", None, &[("a".to_owned(), "b".to_owned())])
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
            // Reads and waits stay available in readonly mode: they only fail at the
            // transport, never with `readonly_mode`.
            assert_eq!(
                client
                    .file_read("notes/demo.txt", None, None, None)
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
            let call = Arc::new(AtomicU64::new(0));
            let server = MockHcServer::start(
                "file-edit",
                2,
                &["hello", "file.edit"],
                seen.clone(),
                move |method, params| {
                    assert_eq!(method, "file.edit");
                    assert_eq!(params["path"], "src/lib.rs");
                    assert_eq!(params["cwd"], "/tmp/work");
                    assert_eq!(
                        params["edits"],
                        json!([
                            {"old": "fn a() {}", "new": "fn a() { 1 }"},
                            {"old": "fn b() {}", "new": "fn b() { 2 }"}
                        ])
                    );
                    if call.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(json!({"path": "/tmp/work/src/lib.rs", "bytes": 42}))
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

            let applied = client
                .file_edit("src/lib.rs", Some(Path::new("/tmp/work")), &edits)
                .unwrap();
            assert_eq!(applied["ok"], true);
            assert_eq!(applied["path"], "/tmp/work/src/lib.rs");
            assert_eq!(applied["bytes"], 42);
            assert_eq!(applied["edits_applied"], 2);

            let failure = client
                .file_edit("src/lib.rs", Some(Path::new("/tmp/work")), &edits)
                .unwrap_err();
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
                .file_read("notes/readme.md", None, None, None)
                .unwrap_err();
            assert_eq!(refused.code, "runtime_capability_unsupported");
            server.finish();
        }

        #[test]
        fn invalid_primitive_arguments_are_refused_before_the_socket() {
            let client = HerdrCoreClient::new(absent_socket_path("invalid-args"));
            assert_eq!(
                client.file_read("   ", None, None, None).unwrap_err().code,
                "invalid_params"
            );
            assert_eq!(
                client
                    .file_read("notes/readme.md", None, Some(0), None)
                    .unwrap_err()
                    .code,
                "invalid_params"
            );
            assert_eq!(
                client
                    .file_write("notes/readme.md", None, b"x")
                    .unwrap_err()
                    .code,
                "herdr_core_unreachable"
            );
            assert_eq!(
                client
                    .file_edit("notes/readme.md", None, &[])
                    .unwrap_err()
                    .code,
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
        fn secret_paths_are_refused_through_symlinks_and_secret_working_directories() {
            let root = unique_tmp_dir("symlinks");
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

            let client = HerdrCoreClient::new(absent_socket_path("symlinks"));
            let notes_abs = root.join("notes.txt");
            let notes_abs = notes_abs.to_str().unwrap();

            // `notes.txt -> .env` with a relative path + cwd, and as an absolute path.
            assert_eq!(
                client
                    .file_read("notes.txt", Some(&root), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            assert_eq!(
                client
                    .file_read(notes_abs, None, None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Dangling symlink to a not-yet-created `.env`.
            assert_eq!(
                client
                    .file_read("later.txt", Some(&root), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // The secret hides in `cwd` while the leaf name looks clean.
            assert_eq!(
                client
                    .file_read("config", Some(&ssh_dir), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // `..` still resolves onto the refused target.
            assert_eq!(
                client
                    .file_read("sub/../.env", Some(&root), None, None)
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Mutations run the same guard.
            assert_eq!(
                client
                    .file_write("notes.txt", Some(&root), b"x")
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            assert_eq!(
                client
                    .file_edit(
                        "notes.txt",
                        Some(&root),
                        &[("a".to_owned(), "b".to_owned())]
                    )
                    .unwrap_err()
                    .code,
                "secret_path_denied"
            );
            // Control: a benign relative file is not refused, so the guard did not
            // start blanket-rejecting relative paths; it stops at the transport.
            assert_eq!(
                client
                    .file_read("readme.md", Some(&root), None, None)
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
                .file_edit("notes/demo.txt", None, &edits)
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
                .file_read("notes/readme.md", None, None, None)
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
                .file_read("notes/big.txt", None, None, None)
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
