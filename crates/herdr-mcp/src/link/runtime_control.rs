//! Staged runtime-control loop for the workstation Link.
//!
//! This layer polls a bounded `runtime-control.json` document, registers the
//! listed generations through [`RuntimeGenerationManager`], and activates the
//! desired generation. Status is written atomically next to the control file.
//!
//! This module does not own CLI `link run`, daemon startup, launchd/service
//! mutation, `runtime/current`, or production Link cutover.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;

use super::runtime_generation::{
    RUNTIME_GENERATION_SCHEMA_VERSION, RuntimeGenerationManager, RuntimeGenerationSpec,
    RuntimeManagerStatus, SpecError, validate_runtime_generation_spec,
};

const MAX_CONTROL_BYTES: u64 = 128 * 1024;
const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    Message(&'static str),
    Owned(String),
    Spec(SpecError),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => write!(f, "{message}"),
            Self::Owned(message) => write!(f, "{message}"),
            Self::Spec(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeControlObservation {
    pub checks: u64,
    pub interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeControlDocument {
    pub schema_version: u64,
    pub revision: u64,
    pub desired_active: String,
    pub generations: Vec<RuntimeGenerationSpec>,
    pub observation: Option<RuntimeControlObservation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeControlStatusDocument {
    pub schema_version: u64,
    pub processed_revision: u64,
    pub desired_active: String,
    pub outcome: String,
    pub updated_at_ms: i64,
    pub manager: RuntimeManagerStatus,
}

impl RuntimeControlStatusDocument {
    fn to_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "processed_revision": self.processed_revision,
            "desired_active": self.desired_active,
            "outcome": self.outcome,
            "updated_at_ms": self.updated_at_ms,
            "manager": manager_status_json(&self.manager),
        })
    }
}

pub struct RuntimeControlLoopOptions {
    pub manager: Arc<RuntimeGenerationManager>,
    pub base: RuntimeGenerationSpec,
    pub control_path: PathBuf,
    pub status_path: PathBuf,
    pub poll_interval_ms: Option<u64>,
    pub now_ms: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

struct Inner {
    manager: Arc<RuntimeGenerationManager>,
    base: RuntimeGenerationSpec,
    control_path: PathBuf,
    status_path: PathBuf,
    poll_interval_ms: u64,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    ticking: AtomicBool,
    processed_revision: AtomicU64,
    stop: tokio::sync::Notify,
    poller: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct RuntimeControlLoop {
    inner: Arc<Inner>,
}

impl RuntimeControlLoop {
    pub fn new(options: RuntimeControlLoopOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                manager: options.manager,
                base: options.base,
                control_path: options.control_path,
                status_path: options.status_path,
                poll_interval_ms: safe_integer(
                    options.poll_interval_ms.map(|value| value as f64),
                    DEFAULT_POLL_INTERVAL_MS,
                    100,
                    60_000,
                ),
                now_ms: options.now_ms.unwrap_or_else(|| {
                    Arc::new(|| {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|duration| duration.as_millis() as i64)
                            .unwrap_or(0)
                    })
                }),
                ticking: AtomicBool::new(false),
                processed_revision: AtomicU64::new(0),
                stop: tokio::sync::Notify::new(),
                poller: Mutex::new(None),
            }),
        }
    }

    pub fn manager(&self) -> &RuntimeGenerationManager {
        &self.inner.manager
    }

    pub fn control_path(&self) -> &Path {
        &self.inner.control_path
    }

    pub fn status_path(&self) -> &Path {
        &self.inner.status_path
    }

    pub async fn initialize(&self) -> Result<(), String> {
        let parent = self.inner.control_path.parent().ok_or_else(|| {
            format!(
                "runtime-control: path has no parent: {}",
                self.inner.control_path.display()
            )
        })?;
        mkdir_private(parent)?;
        if fs::metadata(&self.inner.control_path).is_err() {
            let initial = RuntimeControlDocument {
                schema_version: RUNTIME_GENERATION_SCHEMA_VERSION,
                revision: 1,
                desired_active: self.inner.base.generation.clone(),
                generations: vec![self.inner.base.clone()],
                observation: Some(RuntimeControlObservation {
                    checks: 3,
                    interval_ms: 500,
                }),
            };
            atomic_json(&self.inner.control_path, &control_document_json(&initial))?;
        }
        let _ = self.tick().await;
        Ok(())
    }

    pub fn start(&self) {
        let mut poller = self.inner.poller.lock().expect("runtime-control poller");
        if poller.is_some() {
            return;
        }
        let this = self.clone();
        let interval_ms = this.inner.poll_interval_ms;
        *poller = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {
                        let _ = this.tick().await;
                    }
                    _ = this.inner.stop.notified() => break,
                }
            }
        }));
    }

    pub fn close(&self) {
        self.inner.stop.notify_waiters();
        if let Some(handle) = self
            .inner
            .poller
            .lock()
            .expect("runtime-control poller")
            .take()
        {
            handle.abort();
        }
    }

    pub async fn tick(&self) -> Option<RuntimeControlStatusDocument> {
        if self
            .inner
            .ticking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        let _guard = TickGuard(&self.inner.ticking);
        let doc = match read_control_document(&self.inner.control_path) {
            Ok(doc) => doc,
            Err(error) => {
                let status = self.status(
                    self.inner.processed_revision.load(Ordering::SeqCst),
                    self.inner.manager.active_generation_id(),
                    format!("control_invalid:{error}"),
                );
                let _ = atomic_json(&self.inner.status_path, &status.to_json());
                return Some(status);
            }
        };
        if doc.revision <= self.inner.processed_revision.load(Ordering::SeqCst) {
            return None;
        }

        let mut outcome = "validated".to_owned();
        let desired = doc.desired_active.clone();
        for spec in &doc.generations {
            let current = self.inner.manager.get_generation_spec(&spec.generation);
            if same_spec(current.as_ref(), spec) {
                if spec.generation != self.inner.manager.active_generation_id() {
                    let validation = self
                        .inner
                        .manager
                        .validate_generation(&spec.generation)
                        .await;
                    if !validation.ok && spec.generation == desired {
                        outcome = format!("candidate_rejected:{}", validation.code);
                    }
                }
                continue;
            }
            match self.inner.manager.register_generation(spec.clone()).await {
                Ok(validation) => {
                    if !validation.ok && spec.generation == desired {
                        outcome = format!("candidate_rejected:{}", validation.code);
                    }
                }
                Err(error) if spec.generation == desired => {
                    outcome = format!("candidate_rejected:{error}");
                }
                Err(_) => {}
            }
        }

        if !outcome.starts_with("candidate_rejected")
            && desired != self.inner.manager.active_generation_id()
        {
            let activated = self
                .inner
                .manager
                .activate_generation(
                    &desired,
                    doc.observation
                        .as_ref()
                        .map(|observation| observation.checks),
                    doc.observation
                        .as_ref()
                        .map(|observation| observation.interval_ms),
                )
                .await;
            outcome = if activated.ok {
                "activated".to_owned()
            } else if activated.rolled_back {
                format!("rolled_back:{}", activated.code)
            } else {
                format!("activation_blocked:{}", activated.code)
            };
        } else if desired == self.inner.manager.active_generation_id() && outcome == "validated" {
            outcome = "active_unchanged".to_owned();
        }

        let wanted: HashSet<String> = doc
            .generations
            .iter()
            .map(|spec| spec.generation.clone())
            .collect();
        let active = self.inner.manager.active_generation_id();
        for state in self.inner.manager.get_status().generations {
            if !wanted.contains(&state.generation) && state.generation != active {
                let _ = self.inner.manager.remove_generation(&state.generation);
            }
        }

        let previous_revision = self.inner.processed_revision.load(Ordering::SeqCst);
        let retrying = retryable_candidate_outcome(&outcome);
        let processed_revision = if retrying {
            previous_revision
        } else {
            self.inner
                .processed_revision
                .store(doc.revision, Ordering::SeqCst);
            doc.revision
        };
        let outcome = if retrying {
            format!("retrying:{outcome}")
        } else {
            outcome
        };
        let status = self.status(processed_revision, desired, outcome);
        let _ = atomic_json(&self.inner.status_path, &status.to_json());
        Some(status)
    }

    fn status(
        &self,
        revision: u64,
        desired: String,
        outcome: String,
    ) -> RuntimeControlStatusDocument {
        RuntimeControlStatusDocument {
            schema_version: RUNTIME_GENERATION_SCHEMA_VERSION,
            processed_revision: revision,
            desired_active: desired,
            outcome,
            updated_at_ms: (self.inner.now_ms)(),
            manager: self.inner.manager.get_status(),
        }
    }
}

struct TickGuard<'a>(&'a AtomicBool);

impl Drop for TickGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub fn validate_runtime_control_document(
    value: &Value,
) -> Result<RuntimeControlDocument, ControlError> {
    let object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(ControlError::Message(
                "runtime-control: document must be an object",
            ));
        }
    };
    if json_integer(object.get("schema_version")) != Some(RUNTIME_GENERATION_SCHEMA_VERSION as i64)
    {
        return Err(ControlError::Message(
            "runtime-control: schema_version must be 1",
        ));
    }
    let revision = json_integer(object.get("revision"))
        .filter(|value| *value >= 1)
        .ok_or(ControlError::Message(
            "runtime-control: revision must be a positive integer",
        ))?;
    let desired_active = object
        .get("desired_active")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ControlError::Message(
            "runtime-control: desired_active is required",
        ))?
        .to_owned();
    let generations_value = object.get("generations").ok_or(ControlError::Message(
        "runtime-control: generations must contain 1..8 entries",
    ))?;
    let Value::Array(items) = generations_value else {
        return Err(ControlError::Message(
            "runtime-control: generations must contain 1..8 entries",
        ));
    };
    if !(1..=8).contains(&items.len()) {
        return Err(ControlError::Message(
            "runtime-control: generations must contain 1..8 entries",
        ));
    }
    let mut seen = HashSet::new();
    let mut generations = Vec::with_capacity(items.len());
    for item in items {
        let spec = spec_from_value(item)?;
        if !seen.insert(spec.generation.clone()) {
            return Err(ControlError::Message(
                "runtime-control: duplicate generation id",
            ));
        }
        generations.push(spec);
    }
    if !seen.contains(&desired_active) {
        return Err(ControlError::Owned(
            "runtime-control: desired_active must reference a registered generation".to_owned(),
        ));
    }
    let observation = match object.get("observation") {
        None => None,
        Some(value) => Some(observation_from_value(value)?),
    };
    Ok(RuntimeControlDocument {
        schema_version: RUNTIME_GENERATION_SCHEMA_VERSION,
        revision: revision as u64,
        desired_active,
        generations,
        observation,
    })
}

fn spec_from_value(value: &Value) -> Result<RuntimeGenerationSpec, ControlError> {
    let object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(ControlError::Spec(SpecError::InvalidGenerationId));
        }
    };
    let spec = RuntimeGenerationSpec {
        generation: object
            .get("generation")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        endpoint: object
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        expected_runtime_version: optional_string(object, "expected_runtime_version")?,
        runtime_commit: optional_string(object, "runtime_commit")?,
    };
    validate_runtime_generation_spec(&spec).map_err(ControlError::Spec)?;
    Ok(spec)
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, ControlError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) if key == "expected_runtime_version" => {
            Err(ControlError::Spec(SpecError::InvalidExpectedRuntimeVersion))
        }
        Some(_) => Err(ControlError::Owned(
            "runtime-generation: runtime_commit must be null or a string".to_owned(),
        )),
    }
}

fn observation_from_value(value: &Value) -> Result<RuntimeControlObservation, ControlError> {
    match value {
        Value::Object(object) => Ok(RuntimeControlObservation {
            checks: safe_integer(json_number(object.get("checks")), 3, 1, 20),
            interval_ms: safe_integer(json_number(object.get("interval_ms")), 500, 0, 10_000),
        }),
        Value::Null | Value::Array(_) | Value::String(_) | Value::Number(_) | Value::Bool(_) => {
            Err(ControlError::Message(
                "runtime-control: observation must be an object",
            ))
        }
    }
}

fn same_spec(current: Option<&RuntimeGenerationSpec>, spec: &RuntimeGenerationSpec) -> bool {
    current.is_some_and(|current| {
        current.generation == spec.generation
            && current.endpoint == spec.endpoint
            && current.expected_runtime_version == spec.expected_runtime_version
            && current.runtime_commit == spec.runtime_commit
    })
}

pub(crate) fn retryable_candidate_outcome(outcome: &str) -> bool {
    if outcome == "rolled_back:activation_rolled_back" {
        return true;
    }
    let code = outcome
        .strip_prefix("candidate_rejected:")
        .or_else(|| outcome.strip_prefix("activation_blocked:"));
    let Some(code) = code else {
        return false;
    };
    code.starts_with("health_")
        || matches!(code, "catalog_timeout" | "catalog_unreachable")
        || matches!(
            code.strip_prefix("catalog_http_")
                .and_then(|value| value.parse::<u16>().ok()),
            Some(408 | 425 | 429 | 500 | 502 | 503 | 504 | 524)
        )
}

fn safe_integer(value: Option<f64>, fallback: u64, min: u64, max: u64) -> u64 {
    match value {
        Some(number) if number.fract() == 0.0 && number >= min as f64 && number <= max as f64 => {
            number as u64
        }
        _ => fallback,
    }
}

fn json_integer(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64().or_else(|| {
            number.as_f64().and_then(|value| {
                (value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64)
                    .then_some(value as i64)
            })
        }),
        Some(Value::String(text)) => text.parse::<f64>().ok().and_then(|value| {
            (value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64)
                .then_some(value as i64)
        }),
        _ => None,
    }
}

fn json_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn control_document_json(document: &RuntimeControlDocument) -> Value {
    let mut object = json!({
        "schema_version": document.schema_version,
        "revision": document.revision,
        "desired_active": document.desired_active,
        "generations": document.generations.iter().map(|spec| {
            let mut item = json!({
                "generation": spec.generation,
                "endpoint": spec.endpoint,
            });
            if let Some(version) = &spec.expected_runtime_version {
                item["expected_runtime_version"] = json!(version);
            }
            if let Some(commit) = &spec.runtime_commit {
                item["runtime_commit"] = json!(commit);
            }
            item
        }).collect::<Vec<_>>(),
    });
    if let Some(observation) = &document.observation {
        object["observation"] = json!({
            "checks": observation.checks,
            "interval_ms": observation.interval_ms,
        });
    }
    object
}

fn manager_status_json(status: &RuntimeManagerStatus) -> Value {
    json!({
        "active_generation": status.active_generation,
        "previous_generation": status.previous_generation,
        "last_good_generation": status.last_good_generation,
        "transition_seq": status.transition_seq,
        "last_transition": status.last_transition.as_ref().map(|transition| json!({
            "from": transition.from,
            "to": transition.to,
            "outcome": transition.outcome,
            "reason": transition.reason,
            "at_ms": transition.at_ms,
        })),
        "generations": status.generations.iter().map(|generation| json!({
            "generation": generation.generation,
            "endpoint": generation.endpoint,
            "phase": generation.phase.as_str(),
            "in_flight": generation.in_flight,
            "validation": generation.validation.as_ref().map(|validation| json!({
                "ok": validation.ok,
                "code": validation.code,
                "runtime_version": validation.runtime_version,
                "contract_hash": validation.contract_hash,
                "tool_count": validation.tool_count,
                "checked_at_ms": validation.checked_at_ms,
            })),
        })).collect::<Vec<_>>(),
    })
}

fn read_control_document(path: &Path) -> Result<RuntimeControlDocument, ControlError> {
    let info = fs::metadata(path).map_err(|error| {
        ControlError::Owned(format!("runtime-control: cannot read file: {error}"))
    })?;
    if info.len() > MAX_CONTROL_BYTES {
        return Err(ControlError::Message(
            "runtime-control: file exceeds size limit",
        ));
    }
    let text = fs::read_to_string(path).map_err(|error| {
        ControlError::Owned(format!("runtime-control: cannot read file: {error}"))
    })?;
    let value: Value =
        serde_json::from_str(&text).map_err(|error| ControlError::Owned(error.to_string()))?;
    validate_runtime_control_document(&value)
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("runtime-control: path has no parent: {}", path.display()))?;
    mkdir_private(parent)?;
    let tmp = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    write_private(&tmp, body.as_bytes())?;
    chmod_private(&tmp)?;
    fs::rename(&tmp, path)
        .map_err(|error| format!("cannot activate {}: {error}", path.display()))?;
    chmod_private(path)?;
    Ok(())
}

fn mkdir_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn chmod_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("cannot chmod {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::{
        RuntimeControlLoop, RuntimeControlLoopOptions, retryable_candidate_outcome,
        validate_runtime_control_document,
    };
    use crate::link::runtime_generation::{
        RuntimeGenerationManager, RuntimeGenerationManagerOptions, RuntimeGenerationSpec,
    };
    use crate::relay::contract::compute_contract_hash;

    const TOKEN: &str = "runtime-generation-test-token";
    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn catalog() -> Vec<Value> {
        vec![json!({
            "name": "herdr_inspect",
            "description": "inspect",
            "inputSchema": { "type": "object", "properties": {} },
        })]
    }

    struct MockState {
        version: String,
        catalog: Vec<Value>,
        available: AtomicBool,
    }

    async fn read_http_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await.ok()?;
            if read == 0 {
                return if bytes.is_empty() { None } else { Some(bytes) };
            }
            bytes.extend_from_slice(&buffer[..read]);
            let header_end = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
            let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                return Some(bytes);
            }
        }
    }

    async fn write_json(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    async fn serve_mock(state: Arc<MockState>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let Some(bytes) = read_http_request(&mut stream).await else {
                        return;
                    };
                    if !state.available.load(Ordering::SeqCst) {
                        let response = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = stream.write_all(response).await;
                        let _ = stream.shutdown().await;
                        return;
                    }
                    let text = String::from_utf8_lossy(&bytes);
                    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
                    let parsed: Value = serde_json::from_str(body).unwrap_or(json!({}));
                    let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
                    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                    match method {
                        "server/discover" => {
                            let body = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "serverInfo": {
                                        "name": "herdr",
                                        "version": state.version,
                                    }
                                }
                            });
                            write_json(&mut stream, &body.to_string()).await;
                        }
                        "tools/list" => {
                            let body = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "tools": state.catalog },
                            });
                            write_json(&mut stream, &body.to_string()).await;
                        }
                        _ => {
                            write_json(&mut stream, "{\"error\":\"unexpected\"}").await;
                        }
                    }
                });
            }
        });
        format!("http://{address}/mcp")
    }

    fn test_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-runtime-control-{}-{}",
            std::process::id(),
            TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn runtime_control_document_is_bounded_unique_and_desired_generation_must_exist() {
        let good = validate_runtime_control_document(&json!({
            "schema_version": 1,
            "revision": 2,
            "desired_active": "candidate",
            "generations": [
                { "generation": "stable", "endpoint": "http://127.0.0.1:8772/mcp" },
                { "generation": "candidate", "endpoint": "http://127.0.0.1:8773/mcp" },
            ],
        }))
        .unwrap();
        assert_eq!(good.desired_active, "candidate");

        let missing = validate_runtime_control_document(&json!({
            "schema_version": 1,
            "revision": 1,
            "desired_active": "missing",
            "generations": [
                { "generation": "stable", "endpoint": "http://127.0.0.1:8772/mcp" },
            ],
        }))
        .unwrap_err();
        assert!(missing.to_string().contains("desired_active"));

        let duplicate = validate_runtime_control_document(&json!({
            "schema_version": 1,
            "revision": 1,
            "desired_active": "stable",
            "generations": [
                { "generation": "stable", "endpoint": "http://127.0.0.1:8772/mcp" },
                { "generation": "stable", "endpoint": "http://127.0.0.1:8773/mcp" },
            ],
        }))
        .unwrap_err();
        assert!(duplicate.to_string().contains("duplicate generation id"));
    }

    #[test]
    fn cloudflare_524_catalog_failure_is_retryable() {
        assert!(retryable_candidate_outcome(
            "candidate_rejected:catalog_http_524"
        ));
        assert!(!retryable_candidate_outcome(
            "candidate_rejected:catalog_http_404"
        ));
    }

    #[tokio::test]
    async fn file_control_revision_validates_and_activates_a_candidate_without_restarting_the_manager()
     {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            catalog: catalog(),
            available: AtomicBool::new(true),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            catalog: catalog(),
            available: AtomicBool::new(true),
        });
        let stable_endpoint = serve_mock(Arc::clone(&stable_state)).await;
        let candidate_endpoint = serve_mock(candidate_state).await;
        let mut options = RuntimeGenerationManagerOptions::new(
            RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint.clone(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            TOKEN,
            &hash,
        );
        options.observation_checks = 2;
        options.observation_interval_ms = 0;
        let manager = Arc::new(RuntimeGenerationManager::new(options).expect("manager"));
        let dir = test_dir();
        let control_path = dir.join("control.json");
        let status_path = dir.join("status.json");
        let loop_ = RuntimeControlLoop::new(RuntimeControlLoopOptions {
            manager: Arc::clone(&manager),
            base: RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint.clone(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            control_path: control_path.clone(),
            status_path: status_path.clone(),
            poll_interval_ms: Some(100),
            now_ms: None,
        });
        loop_.initialize().await.expect("initialize");
        assert_eq!(manager.active_generation_id(), "stable");

        fs::write(
            &control_path,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "revision": 2,
                "desired_active": "candidate",
                "generations": [
                    {
                        "generation": "stable",
                        "endpoint": stable_endpoint,
                        "expected_runtime_version": "0.3.23",
                    },
                    {
                        "generation": "candidate",
                        "endpoint": candidate_endpoint,
                        "expected_runtime_version": "0.3.26",
                    },
                ],
                "observation": { "checks": 1, "interval_ms": 0 },
            }))
            .unwrap(),
        )
        .unwrap();
        let applied = loop_.tick().await.expect("tick");
        assert_eq!(applied.outcome, "activated");
        assert_eq!(manager.active_generation_id(), "candidate");
        let saved: Value =
            serde_json::from_str(&fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(saved["processed_revision"], 2);
        assert_eq!(saved["manager"]["active_generation"], "candidate");
        loop_.close();
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn transient_candidate_rejection_retries_the_same_revision_and_recovers() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            catalog: catalog(),
            available: AtomicBool::new(true),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            catalog: catalog(),
            available: AtomicBool::new(false),
        });
        let stable_endpoint = serve_mock(stable_state).await;
        let candidate_endpoint = serve_mock(Arc::clone(&candidate_state)).await;
        let mut options = RuntimeGenerationManagerOptions::new(
            RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint.clone(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            TOKEN,
            &hash,
        );
        options.observation_checks = 1;
        options.observation_interval_ms = 0;
        let manager = Arc::new(RuntimeGenerationManager::new(options).expect("manager"));
        let dir = test_dir();
        let control_path = dir.join("control.json");
        let status_path = dir.join("status.json");
        let loop_ = RuntimeControlLoop::new(RuntimeControlLoopOptions {
            manager: Arc::clone(&manager),
            base: RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint.clone(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            control_path: control_path.clone(),
            status_path: status_path.clone(),
            poll_interval_ms: Some(100),
            now_ms: None,
        });
        loop_.initialize().await.expect("initialize");
        fs::write(
            &control_path,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "revision": 2,
                "desired_active": "candidate",
                "generations": [
                    {
                        "generation": "stable",
                        "endpoint": stable_endpoint,
                        "expected_runtime_version": "0.3.23",
                    },
                    {
                        "generation": "candidate",
                        "endpoint": candidate_endpoint,
                        "expected_runtime_version": "0.3.26",
                    },
                ],
                "observation": { "checks": 1, "interval_ms": 0 },
            }))
            .unwrap(),
        )
        .unwrap();

        let rejected = loop_.tick().await.expect("first tick");
        assert!(
            rejected
                .outcome
                .starts_with("retrying:candidate_rejected:health_")
        );
        assert_eq!(rejected.processed_revision, 1);
        assert_eq!(manager.active_generation_id(), "stable");

        candidate_state.available.store(true, Ordering::SeqCst);
        let recovered = loop_.tick().await.expect("retry tick");
        assert_eq!(recovered.outcome, "activated");
        assert_eq!(recovered.processed_revision, 2);
        assert_eq!(manager.active_generation_id(), "candidate");

        loop_.close();
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn invalid_control_document_writes_control_invalid_status() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            catalog: catalog(),
            available: AtomicBool::new(true),
        });
        let stable_endpoint = serve_mock(stable_state).await;
        let mut options = RuntimeGenerationManagerOptions::new(
            RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint.clone(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            TOKEN,
            &hash,
        );
        options.observation_interval_ms = 0;
        let manager = Arc::new(RuntimeGenerationManager::new(options).expect("manager"));
        let dir = test_dir();
        let control_path = dir.join("control.json");
        let status_path = dir.join("status.json");
        let loop_ = RuntimeControlLoop::new(RuntimeControlLoopOptions {
            manager: Arc::clone(&manager),
            base: RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: stable_endpoint,
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            control_path: control_path.clone(),
            status_path: status_path.clone(),
            poll_interval_ms: Some(100),
            now_ms: None,
        });
        loop_.initialize().await.expect("initialize");
        fs::write(&control_path, b"{\"schema_version\":1,\"revision\":2}").unwrap();
        let applied = loop_.tick().await.expect("tick");
        assert!(applied.outcome.starts_with("control_invalid:"));
        assert_eq!(manager.active_generation_id(), "stable");
        loop_.close();
        let _ = fs::remove_dir_all(&dir);
    }
}
