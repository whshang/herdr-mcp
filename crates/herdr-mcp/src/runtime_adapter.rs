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
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const HERDR_CORE_PROTOCOL_GENERATION: u32 = 1;
pub const RUNTIME_BACKEND_ENV: &str = "HERDR_MCP_RUNTIME_BACKEND";

const HC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HC_EXEC_MAX_TIMEOUT: Duration = Duration::from_secs(60);
const HC_MAX_LINE_BYTES: usize = 1024 * 1024;
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
            if trimmed.is_empty()
                || trimmed.len() > 64
                || !trimmed
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(format!(
                    "invalid HC_SESSION '{trimmed}': must be 1..=64 ASCII [A-Za-z0-9._-] characters"
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
        self.with_negotiated_session(|hello, stream| {
            let capabilities = RuntimeCapabilities::from_herdr_core_methods(&hello.methods);
            if !capabilities.server_status {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: "herdr-core hello did not advertise server.status".to_owned(),
                });
            }
            let status = rpc_call_line(stream, &self.socket_path, "server.status", json!({}))?;
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
            let status = rpc_call_line(stream, &self.socket_path, "server.status", json!({}))?;
            let list = rpc_call_line(
                stream,
                &self.socket_path,
                "terminal.list",
                json!({"all": false}),
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
        self.with_negotiated_session(|hello, stream| {
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
            if let Some(timeout_secs) = timeout_secs {
                params.insert("timeout".to_owned(), json!(timeout_secs.clamp(1, 600)));
            }
            let raw = rpc_call_line(
                stream,
                &self.socket_path,
                "machine.exec",
                Value::Object(params),
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
        self.with_negotiated_session(|hello, stream| {
            if !hello.methods.contains(method) {
                return Err(HerdrCoreError {
                    code: "runtime_capability_unsupported".to_owned(),
                    kind: None,
                    message: format!("herdr-core hello.methods did not advertise '{method}'"),
                });
            }
            rpc_call_line(stream, &self.socket_path, method, params)
        })
    }

    fn with_negotiated_session<T>(
        &self,
        op: impl FnOnce(HerdrCoreHello, &mut HcSocketStream) -> Result<T, HerdrCoreError>,
    ) -> Result<T, HerdrCoreError> {
        let mut stream = connect_hc_socket(&self.socket_path, self.timeout)?;
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
    stream: &mut (impl Read + Write),
    socket_path: &Path,
    method: &str,
    params: Value,
) -> Result<Value, HerdrCoreError> {
    let id = NEXT_HC_RPC_ID.fetch_add(1, Ordering::Relaxed);
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
    stream
        .write_all(&encoded)
        .map_err(|error| map_hc_io_error(error, socket_path))?;

    let line = read_hc_line(stream)?;
    let envelope: Value = serde_json::from_slice(&line).map_err(|error| HerdrCoreError {
        code: "parse_error".to_owned(),
        kind: None,
        message: error.to_string(),
    })?;

    if let Some(error) = envelope.get("error") {
        let rpc_code = error.get("code").and_then(Value::as_i64);
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
        let code = if rpc_code == Some(-32000) || kind.as_deref() == Some("unsupported_generation")
        {
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

fn read_hc_line(reader: &mut impl Read) -> Result<Vec<u8>, HerdrCoreError> {
    let mut output = Vec::with_capacity(1024);
    let mut byte = [0_u8; 1];
    loop {
        let count = reader.read(&mut byte).map_err(|error| HerdrCoreError {
            code: if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                "timeout".to_owned()
            } else {
                "herdr_core_socket_error".to_owned()
            },
            kind: None,
            message: error.to_string(),
        })?;
        if count == 0 {
            return Err(HerdrCoreError {
                code: "unexpected_eof".to_owned(),
                kind: None,
                message: "herdr-core socket closed before newline-delimited JSON-RPC reply"
                    .to_owned(),
            });
        }
        if byte[0] == b'\n' {
            return Ok(output);
        }
        output.push(byte[0]);
        if output.len() > HC_MAX_LINE_BYTES {
            return Err(HerdrCoreError {
                code: "response_too_large".to_owned(),
                kind: None,
                message: format!("herdr-core reply exceeded {HC_MAX_LINE_BYTES} bytes"),
            });
        }
    }
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

        let traversal = EnvOverrides {
            runtime_backend: Some("herdr-core".to_owned()),
            hc_session: Some("../escape".to_owned()),
            xdg_state_home: Some(PathBuf::from("/tmp/xdg-state-test")),
            ..EnvOverrides::default()
        };
        assert!(resolve_backend_selection_with_env(None, false, &traversal).is_err());
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
}
