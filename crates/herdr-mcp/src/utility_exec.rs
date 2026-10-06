use crate::exec_evidence;
use crate::exec_sessions::ExecRegistry;
use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::projects;
use crate::state_store::{OperationReservation, StateStore};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const UTILITY_LABEL: &str = "herdr-mcp:utility";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const PRE_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
const UTILITY_OWNED_WAIT: Duration = Duration::from_secs(2);
const UTILITY_OWNED_POLL: Duration = Duration::from_millis(100);
const STALE_SCRIPT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STRUCTURED_STEPS: usize = 16;
const MAX_STRUCTURED_ARGS: usize = 128;
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 256;
/// Durable retention for one `herdr_exec` idempotency record. It must be at
/// least the exec-session retention (`exec_sessions::SESSION_TTL_MS`, 60
/// minutes): an `exec_timeout` child can still be running as the returned
/// session_id after the row would otherwise expire, and a shorter window would
/// let the same key launch a duplicate of that live process.
const EXEC_RECORD_TTL_MS: u64 = 60 * 60_000;
/// Replay payload bound; a synchronous exec result stays well under this.
const MAX_EXEC_REPLAY_JSON_BYTES: usize = 128 * 1024;
/// Durable operations-ledger kind for `herdr_exec` idempotency.
const EXEC_OPERATION_KIND: &str = "herdr_exec";
static UTILITY_SUBMISSION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static UTILITY_PANE_IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

#[derive(Debug, Clone)]
struct WorkspaceRecord {
    id: String,
}

#[derive(Debug, Clone)]
struct PaneRecord {
    id: String,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct PaneReadiness {
    ready: bool,
    shell_pid: Option<u64>,
    foreground_process_group_id: Option<u64>,
    foreground: Vec<Value>,
}

#[derive(Debug)]
enum PrepareError {
    ControlPlane(String),
    Other { code: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StructuredExecStep {
    program: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone)]
enum ExecInvocation {
    Command(String),
    Steps(Vec<StructuredExecStep>),
}

impl ExecInvocation {
    fn rendered(&self) -> String {
        match self {
            Self::Command(command) => command.clone(),
            Self::Steps(steps) => steps
                .iter()
                .map(|step| {
                    std::iter::once(step.program.as_str())
                        .chain(step.args.iter().map(String::as_str))
                        .map(shell_quote)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join(" && "),
        }
    }
}

fn pre_start_rejection(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object
            .entry("delivery_state".to_owned())
            .or_insert_with(|| json!("not_delivered"));
        exec_evidence::insert_control_plane_rejection(object);
    }
    result
}

/// Declared effect of an opt-in durable `herdr_exec` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecIntent {
    ReadOnly,
    IdempotentWrite,
    NonIdempotentWrite,
}

impl ExecIntent {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "read_only" => Some(Self::ReadOnly),
            "idempotent_write" => Some(Self::IdempotentWrite),
            "non_idempotent_write" => Some(Self::NonIdempotentWrite),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::IdempotentWrite => "idempotent_write",
            Self::NonIdempotentWrite => "non_idempotent_write",
        }
    }
}

/// Opt-in durability request parsed from `herdr_exec` arguments.
///
/// No fields is the legacy path and reserves nothing. A declared intent is
/// always accepted: a write intent needs a key, `read_only` may omit one. A
/// supplied key always needs an intent.
#[derive(Debug, Clone, Copy)]
struct ExecRequest<'a> {
    intent: Option<ExecIntent>,
    key: Option<&'a str>,
}

impl<'a> ExecRequest<'a> {
    fn parse(args: &'a Value) -> Result<Self, Value> {
        let intent = match optional_str(args, "intent")? {
            None => None,
            Some("") => return Err(invalid("intent must not be empty")),
            Some(value) => Some(ExecIntent::parse(value).ok_or_else(|| {
                invalid("intent must be one of read_only, idempotent_write, non_idempotent_write")
            })?),
        };
        let key = match optional_str(args, "idempotency_key")? {
            Some("") => return Err(invalid("idempotency_key must not be empty")),
            Some(key) if key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS => {
                return Err(invalid(&format!(
                    "idempotency_key must be at most {MAX_IDEMPOTENCY_KEY_CHARS} characters"
                )));
            }
            other => other,
        };
        match (intent, key) {
            (None, None) => Ok(Self {
                intent: None,
                key: None,
            }),
            (None, Some(_)) => Err(invalid(
                "idempotency_key requires an explicit intent (read_only, idempotent_write, or non_idempotent_write)",
            )),
            (Some(_), None) if intent != Some(ExecIntent::ReadOnly) => Err(invalid(
                "a write intent requires idempotency_key so the request can be replayed durably",
            )),
            (intent, key) => Ok(Self { intent, key }),
        }
    }
}

fn exec_key_hash(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"herdr_exec/idempotency");
    hasher.update(key.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn exec_now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

/// Canonical identity of the resolved invocation including its execution mode.
/// A freeform command and a structured step list can render identically while
/// using different paths, and a structured argument may contain any separator
/// other than NUL, so JSON field boundaries rather than a delimiter join keep
/// distinct requests distinct. Never echoed in results.
fn invocation_identity(invocation: &ExecInvocation) -> String {
    match invocation {
        ExecInvocation::Command(command) => json!({"mode": "command", "command": command}),
        ExecInvocation::Steps(steps) => json!({
            "mode": "steps",
            "steps": steps
                .iter()
                .map(|step| json!({"program": step.program, "args": step.args}))
                .collect::<Vec<_>>(),
        }),
    }
    .to_string()
}

/// Binds the resolved workspace/root, the invocation and its mode, the intent
/// and the busy/timeout gates, so a key can only replay an identical request.
fn exec_request_fingerprint(
    workspace_id: &str,
    effective_root: &Path,
    invocation: &ExecInvocation,
    intent: Option<ExecIntent>,
    confirm_busy: bool,
    timeout_ms: u64,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        workspace_id,
        &effective_root.to_string_lossy(),
        &invocation_identity(invocation),
        intent.map(ExecIntent::as_str).unwrap_or("legacy"),
        if confirm_busy { "1" } else { "0" },
        &timeout_ms.to_string(),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn idempotency_error(code: &str, message: impl Into<String>) -> Value {
    pre_start_rejection(json!({
        "ok": false,
        "code": code,
        "message": message.into(),
        "retryable": false,
    }))
}

/// Reserve `(herdr_exec, idempotency_key)` before any delivery. `Ok(None)`
/// means this key is reserved for this request; `Ok(Some(replay))` returns the
/// recorded synchronous result of an already-settled key. A same-key/different
/// request and a still-pending row both fail closed without executing.
fn reserve_durable_exec(
    store: &Arc<Mutex<StateStore>>,
    key: &str,
    fingerprint: &str,
    intent: Option<ExecIntent>,
) -> Result<Option<Value>, Value> {
    let now = exec_now_ms();
    let key_hash = exec_key_hash(key);
    let op_id = format!("op:exec:{}", &key_hash[..32]);
    let unavailable = |message: String| {
        idempotency_error(
            "idempotency_store_unavailable",
            format!("{message}; the command was not executed"),
        )
    };
    let mut store = store
        .lock()
        .map_err(|_| unavailable("durable idempotency store lock is unavailable".to_owned()))?;
    let reservation = store
        .reserve_operation(
            EXEC_OPERATION_KIND,
            &key_hash,
            fingerprint,
            &op_id,
            now,
            // A reservation that may already have been delivered must never be
            // pruned by time alone, so the ownership claim carries no expiry.
            None,
        )
        .map_err(|error| unavailable(format!("durable idempotency reservation failed: {error}")))?;
    let record = match reservation {
        OperationReservation::Reserved => return Ok(None),
        OperationReservation::Existing(record) => record,
    };
    if record.request_hash != fingerprint {
        return Err(pre_start_rejection(json!({
            "ok": false,
            "code": "idempotency_key_conflict",
            "message": "idempotency_key is already bound to a different herdr_exec request",
            "retryable": false,
            "op_id": record.op_id,
            "delivery_state": "not_delivered",
        })));
    }
    match record.state.as_deref() {
        // Pending stays fail-closed; herdr_exec reservations do not expire by time.
        Some("pending") => {
            let mut result = json!({
                "ok": false,
                "code": "idempotency_in_flight",
                "message": "a herdr_exec request with this idempotency_key is reserved and may already have been delivered",
                "delivery_state": "unknown",
                "retryable": false,
                "op_id": record.op_id,
                "hint": "A durable reservation exists without a settled outcome. Inspect existing exec sessions before another delivery; a runtime restart does not clear the reservation.",
            });
            if let Some(object) = result.as_object_mut() {
                exec_evidence::insert_uncertain_start(object);
            }
            Err(result)
        }
        Some("complete") => {
            let corrupt =
                |message: String| idempotency_error("idempotency_record_corrupt", message);
            let result_json = record.result_json.ok_or_else(|| {
                corrupt("completed idempotency record has no replay payload".into())
            })?;
            let mut replay: Value = serde_json::from_str(&result_json).map_err(|error| {
                corrupt(format!(
                    "completed idempotency replay payload is invalid: {error}"
                ))
            })?;
            let object = replay.as_object_mut().ok_or_else(|| {
                corrupt("completed idempotency replay payload is not an object".into())
            })?;
            if let Some(intent) = intent {
                object.insert("intent".to_owned(), json!(intent.as_str()));
            }
            object.insert("idempotent_replay".to_owned(), json!(true));
            object.insert("idempotency_persisted".to_owned(), json!(true));
            // The stored synchronous result keeps its own op_id/session_id;
            // the replay adds metadata only and never rewrites identity.
            // A settled row that is not a completed process must never imply a
            // blind retry; the caller resumes through the recorded session_id.
            let retry_mode = if object.get("phase").and_then(Value::as_str) == Some("completed") {
                "none"
            } else {
                "inspect_before_retry"
            };
            object.insert("safe_retry_mode".to_owned(), json!(retry_mode));
            Ok(Some(replay))
        }
        other => Err(idempotency_error(
            "idempotency_record_corrupt",
            format!("idempotency record has unsupported state {other:?}"),
        )),
    }
}

/// Release a reservation this request just inserted after the pre-delivery
/// mutation gate rejected it. It deletes exactly the row created for this
/// key/fingerprint/derived op_id, so a later identical request is not blocked
/// by a pending reservation that never delivered anything. Any mismatch is
/// surfaced to the caller as a fail-closed store error.
fn release_durable_exec(
    store: &Arc<Mutex<StateStore>>,
    key: &str,
    fingerprint: &str,
) -> Result<(), String> {
    let key_hash = exec_key_hash(key);
    let op_id = format!("op:exec:{}", &key_hash[..32]);
    let mut store = store
        .lock()
        .map_err(|_| "durable idempotency store lock is unavailable".to_owned())?;
    store.release_operation_reservation(EXEC_OPERATION_KIND, &key_hash, fingerprint, &op_id)
}

/// Persist the first synchronous submission result. A failure here is reported
/// on the real execution result rather than pretending the command did not run.
///
/// A result with explicit terminal evidence is bounded by the record TTL. Any
/// other shape — notably a bounded-wait timeout whose child may still be
/// running — is stored without an expiry so time alone can never release the
/// ownership claim and allow a duplicate delivery.
fn complete_durable_exec(
    store: &Arc<Mutex<StateStore>>,
    key: &str,
    fingerprint: &str,
    result: &Value,
) -> Result<(), String> {
    let result_json = serde_json::to_string(result)
        .map_err(|error| format!("cannot encode herdr_exec replay payload: {error}"))?;
    if result_json.len() > MAX_EXEC_REPLAY_JSON_BYTES {
        return Err(format!(
            "herdr_exec replay payload exceeds {MAX_EXEC_REPLAY_JSON_BYTES} bytes"
        ));
    }
    let now = exec_now_ms();
    let mut store = store
        .lock()
        .map_err(|_| "durable idempotency store lock is unavailable".to_owned())?;
    store
        .complete_operation(
            EXEC_OPERATION_KIND,
            &exec_key_hash(key),
            fingerprint,
            &result_json,
            now,
            exec_completion_expiry(result, now),
        )
        .map_err(|error| format!("durable idempotency completion failed: {error}"))
}

/// Terminal evidence for a recorded synchronous `herdr_exec` submission.
/// `execution.started == false` is terminal because the command was proven not
/// delivered; `execution.completed == true` and `phase == "completed"` are the
/// completed shapes. Everything else is treated as still uncertain.
fn exec_completion_expiry(result: &Value, now: i64) -> Option<i64> {
    let execution = result.get("execution");
    let terminal = result.get("phase").and_then(Value::as_str) == Some("completed")
        || execution
            .and_then(|value| value.get("completed"))
            .and_then(Value::as_bool)
            == Some(true)
        || execution
            .and_then(|value| value.get("started"))
            .and_then(Value::as_bool)
            == Some(false);
    terminal.then(|| now.saturating_add(EXEC_RECORD_TTL_MS as i64))
}

/// Attach the bounded durability metadata a declared request always exposes.
/// `persisted` is false for a declared intent that reserved nothing.
fn annotate_durable_result(
    mut result: Value,
    intent: Option<ExecIntent>,
    persisted: bool,
) -> Value {
    let settled = persisted && result.get("phase").and_then(Value::as_str) == Some("completed");
    if let Some(object) = result.as_object_mut() {
        if let Some(intent) = intent {
            object.insert("intent".to_owned(), json!(intent.as_str()));
        }
        object.insert("idempotency_persisted".to_owned(), json!(persisted));
        object.insert("idempotent_replay".to_owned(), json!(false));
        object.insert(
            "safe_retry_mode".to_owned(),
            json!(if settled {
                "none"
            } else {
                "inspect_before_retry"
            }),
        );
    }
    result
}

pub fn run_durable(
    client: &HerdrClient,
    snapshot: &Value,
    registry: &ExecRegistry,
    store: &Arc<Mutex<StateStore>>,
    args: &Value,
) -> Value {
    let request = match ExecRequest::parse(args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let workspace_target = match required_str(args, "workspace") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let invocation = match resolve_exec_invocation(args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let command = invocation.rendered();
    let project_root = match optional_str(args, "project_root") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let timeout_ms = match optional_u64(args, "timeout_ms", 1, MAX_TIMEOUT_MS) {
        Ok(value) => value.unwrap_or(DEFAULT_TIMEOUT_MS),
        Err(error) => return error,
    };
    let confirm_busy = match optional_bool(args, "confirm_busy") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let Some(workspace) = resolve_workspace(snapshot, workspace_target) else {
        return pre_start_rejection(json!({
            "ok": false,
            "reason": "workspace_not_found",
            "workspace": workspace_target,
        }));
    };
    let topology = projects::derive_routing(snapshot);
    let current_projects = projects::projects_for_workspace(&topology, &workspace.id);
    let roots = current_projects
        .iter()
        .map(|project| project.root.clone())
        .collect::<Vec<_>>();
    let effective_root = match select_project_root(project_root, &roots) {
        Ok(Some(root)) => root,
        Ok(None) if roots.is_empty() => {
            return pre_start_rejection(json!({
                "ok": false,
                "reason": "project_root_required",
                "workspace": workspace.id,
                "candidates": [],
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "This workspace has no current project root; project_root requires a current attached project.",
            }));
        }
        Ok(None) => {
            return pre_start_rejection(json!({
                "ok": false,
                "reason": "project_root_required",
                "workspace": workspace.id,
                "candidates": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "This workspace has multiple project roots; project_root must match one of the returned candidates.",
            }));
        }
        Err(wanted) => {
            return pre_start_rejection(json!({
                "ok": false,
                "reason": "project_root_not_in_workspace",
                "workspace": workspace.id,
                "project_root": wanted.to_string_lossy(),
                "candidates": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "project_root must match one of this workspace's returned project-root candidates.",
            }));
        }
    };
    // Opt-in durable idempotency, resolved and reserved before the live busy
    // gate. An already-settled replay and an existing pending/conflict outcome
    // both deliver no command, so they must return regardless of later busy
    // state. A newly Reserved key still has to pass the pre-delivery mutation
    // gate below before anything is delivered. A declared intent with no key
    // (`read_only`) reserves nothing and is served as an ordinary request that
    // merely reports its intent.
    let mut reservation: Option<(&str, String)> = None;
    if let Some(key) = request.key {
        let fingerprint = exec_request_fingerprint(
            &workspace.id,
            &effective_root,
            &invocation,
            request.intent,
            confirm_busy,
            timeout_ms,
        );
        match reserve_durable_exec(store, key, &fingerprint, request.intent) {
            Ok(Some(replay)) => return replay,
            Ok(None) => reservation = Some((key, fingerprint)),
            Err(error) => return error,
        }
    }

    let working = match mutation::check_with_topology(
        snapshot,
        &topology,
        &effective_root,
        confirm_busy,
    ) {
        Ok(working) => working,
        Err(error) => {
            // A newly inserted reservation never delivered anything, so it
            // must be released rather than left pending to block a later
            // identical request. A failed release is reported as an
            // explicit fail-closed outcome, not a misleading busy response.
            if let Some((key, fingerprint)) = reservation.as_ref()
                && let Err(release_error) = release_durable_exec(store, key, fingerprint)
            {
                return idempotency_error(
                    "idempotency_store_unavailable",
                    format!(
                        "the pre-delivery mutation gate rejected the request and its pending reservation could not be released ({release_error}); the command was not delivered"
                    ),
                );
            }
            return pre_start_rejection(error);
        }
    };

    // Ordinary, non-interactive execution reuses the durable native
    // `ExecRegistry` backend as start + bounded synchronous wait + result. The
    // visible utility pane is retained only for macOS protected user paths,
    // where the rotating runtime must not become the TCC responsible client.
    // Both backends share one session registry, so a timed-out command stays
    // readable through `herdr_exec_read` and is never re-sent.
    if crate::macos_permissions::project_path_needs_protected_transport(&effective_root) {
        #[cfg(unix)]
        {
            let _ = cleanup_stale_scripts();
            return finish_durable(
                store,
                reservation,
                request.intent,
                run_unix_durable(
                    client,
                    snapshot,
                    registry,
                    (&workspace.id, &effective_root),
                    &command,
                    timeout_ms,
                    &working,
                ),
            );
        }
        #[cfg(not(unix))]
        {
            return finish_durable(
                store,
                reservation,
                request.intent,
                pre_start_rejection(json!({
                    "ok": false,
                    "code": "unsupported_platform",
                    "message": "protected-path execution requires the macOS utility-pane transport",
                    "workspace": workspace.id,
                    "command": command,
                    "effective_cwd": effective_root.to_string_lossy(),
                    "project_root": effective_root.to_string_lossy(),
                })),
            );
        }
    }

    #[cfg(not(unix))]
    let _ = (client, snapshot);

    finish_durable(
        store,
        reservation,
        request.intent,
        run_native_durable(
            registry,
            (&workspace.id, &effective_root),
            &invocation,
            &command,
            timeout_ms,
            &working,
        ),
    )
}

/// Persist the first synchronous submission result for a reserved key.
///
/// The row records that the synchronous `herdr_exec` submission result was
/// captured, not that a timed-out child process later exited. A persistence
/// failure after execution is reported on the real result instead of implying a
/// blind retry. A declared intent that reserved nothing reports its intent with
/// no durable record claimed.
fn finish_durable(
    store: &Arc<Mutex<StateStore>>,
    reservation: Option<(&str, String)>,
    intent: Option<ExecIntent>,
    result: Value,
) -> Value {
    let Some((key, fingerprint)) = reservation else {
        return match intent {
            Some(intent) => annotate_durable_result(result, Some(intent), false),
            None => result,
        };
    };
    let result = annotate_durable_result(result, intent, true);
    match complete_durable_exec(store, key, &fingerprint, &result) {
        Ok(()) => result,
        Err(error) => annotate_completion_failure(result, error),
    }
}

/// The command already ran, so the real execution result stays truthful and the
/// missing replay record is reported as metadata. The reservation usually stays
/// pending/in-flight, which itself blocks a duplicate delivery, so a blind retry
/// is never implied.
fn annotate_completion_failure(mut result: Value, error: String) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("idempotency_persisted".to_owned(), json!(false));
        object.insert("safe_retry_mode".to_owned(), json!("inspect_before_retry"));
        object.insert(
            "idempotency_warning".to_owned(),
            json!(format!(
                "{error}; the completion result was not persisted and the durable reservation may still be pending/in-flight. Inspect the session/process state before retrying, and do not retry blindly."
            )),
        );
    }
    result
}

fn resolve_exec_invocation(args: &Value) -> Result<ExecInvocation, Value> {
    let command = optional_str(args, "command")?;
    let steps = args.get("steps");
    match (command, steps) {
        (Some(""), _) => Err(invalid("command must not be empty")),
        (Some(_), Some(_)) => Err(invalid("exactly one of command or steps is allowed")),
        (Some(command), None) => Ok(ExecInvocation::Command(command.to_owned())),
        (None, Some(steps)) => structured_steps(steps).map(ExecInvocation::Steps),
        (None, None) => Err(invalid("exactly one of command or steps is required")),
    }
}

fn structured_steps(value: &Value) -> Result<Vec<StructuredExecStep>, Value> {
    let steps: Vec<StructuredExecStep> = serde_json::from_value(value.clone())
        .map_err(|_| invalid("steps must be an array of {program,args} objects"))?;
    if steps.is_empty() || steps.len() > MAX_STRUCTURED_STEPS {
        return Err(invalid(&format!(
            "steps must contain 1..={MAX_STRUCTURED_STEPS} items"
        )));
    }
    for (index, step) in steps.iter().enumerate() {
        if step.program.is_empty() || step.program.contains('\0') {
            return Err(invalid(&format!(
                "steps[{index}].program must be non-empty and contain no NUL"
            )));
        }
        if step.args.len() > MAX_STRUCTURED_ARGS || step.args.iter().any(|arg| arg.contains('\0')) {
            return Err(invalid(&format!(
                "steps[{index}].args must contain at most {MAX_STRUCTURED_ARGS} NUL-free strings"
            )));
        }
    }
    Ok(steps)
}

pub(crate) fn start_private_pane_session(
    client: &HerdrClient,
    registry: &ExecRegistry,
    workspace_id: &str,
    effective_root: &Path,
    command: &str,
) -> Value {
    #[cfg(windows)]
    {
        let _ = (client, registry, workspace_id, effective_root, command);
        return json!({
            "ok": false,
            "code": "unsupported_platform",
            "message": "private pane execution requires the Windows Herdr named-pipe transport, which is still pending",
        });
    }

    #[cfg(unix)]
    {
        let pane = match client.call_with_timeout(
            "pane.split",
            json!({
                "workspace_id": workspace_id,
                "direction": "right",
                "cwd": effective_root.to_string_lossy(),
                "focus": false,
            }),
            SPLIT_TIMEOUT,
        ) {
            Ok(value) => value,
            Err(error) => {
                return private_pane_control_plane_error(
                    workspace_id,
                    command,
                    format!("cannot create private exec pane: {}", error.message),
                );
            }
        };
        let Some(pane_id) = extract_pane_id(&pane) else {
            return private_pane_control_plane_error(
                workspace_id,
                command,
                "pane.split returned no pane id".to_owned(),
            );
        };
        let _ = client.call_with_timeout(
            "pane.rename",
            json!({"pane_id": pane_id, "label": "herdr-mcp:exec"}),
            PRE_SEND_TIMEOUT,
        );
        if let Err(error) = client.call_with_timeout(
            "pane.wait_for_output",
            json!({
                "pane_id": pane_id,
                "source": "recent_unwrapped",
                "match": {"type": "regex", "value": "[%#$>❯] ?$"},
                "timeout_ms": 5_000,
            }),
            Duration::from_secs(6),
        ) {
            let _ = client.call_with_timeout(
                "pane.close",
                json!({"pane_id": pane_id}),
                PRE_SEND_TIMEOUT,
            );
            return private_pane_control_plane_error(
                workspace_id,
                command,
                format!(
                    "private exec pane shell did not become ready: {}",
                    error.message
                ),
            );
        }

        match registry.start_in_private_pane(effective_root, command, &pane_id) {
            Ok(mut value) => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("workspace".to_owned(), json!(workspace_id));
                    object.insert("created_exec_pane".to_owned(), json!(true));
                }
                value
            }
            Err(error) => {
                if error.stage.proves_nothing_delivered() {
                    let _ = client.call_with_timeout(
                        "pane.close",
                        json!({"pane_id": pane_id}),
                        PRE_SEND_TIMEOUT,
                    );
                }
                private_pane_start_failure(workspace_id, &pane_id, command, error)
            }
        }
    }
}

#[cfg(unix)]
fn run_unix_durable(
    client: &HerdrClient,
    snapshot: &Value,
    registry: &ExecRegistry,
    target: (&str, &Path),
    command: &str,
    timeout_ms: u64,
    working: &[Value],
) -> Value {
    let (workspace_id, effective_root) = target;
    let started = Instant::now();
    let submission_guard = UTILITY_SUBMISSION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (pane_id, created) =
        match prepare_utility_pane(client, snapshot, workspace_id, effective_root) {
            Ok(value) => value,
            Err(PrepareError::ControlPlane(message)) => {
                return utility_pane_control_plane_error(workspace_id, command, message);
            }
            Err(PrepareError::Other { code, message }) => {
                let mut result = Map::new();
                result.insert("ok".to_owned(), json!(false));
                result.insert("code".to_owned(), json!(code));
                result.insert("message".to_owned(), json!(message));
                result.insert("backend".to_owned(), json!("utility_pane"));
                result.insert("workspace".to_owned(), json!(workspace_id));
                result.insert("command".to_owned(), json!(command));
                result.insert("delivery_state".to_owned(), json!("not_delivered"));
                result.insert(
                    "hint".to_owned(),
                    json!("failed to prepare utility pane before command delivery"),
                );
                exec_evidence::insert_control_plane_rejection(&mut result);
                return Value::Object(result);
            }
        };

    if let Err(result) = ensure_utility_pane_ready(
        client,
        registry,
        workspace_id,
        &pane_id,
        command,
        UTILITY_OWNED_WAIT,
    ) {
        return result;
    }

    let start = match registry.start_in_existing_pane(effective_root, command, &pane_id) {
        Ok(value) => value,
        Err(message) => {
            return utility_pane_start_failure(workspace_id, &pane_id, command, message);
        }
    };
    let Some(session_id) = start.get("session_id").and_then(Value::as_str) else {
        return json!({"ok": false, "code": "exec_start_failed", "message": "durable exec start returned no session_id"});
    };
    // Submission ownership has transferred to the durable session. The
    // synchronous wait below must not serialize unrelated later submissions.
    drop(submission_guard);
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let read = registry.read(session_id, "both", 0, 65_536);
        let completed = read.get("phase").and_then(Value::as_str) == Some("completed");
        if completed {
            let exit_code = read.get("exit_code").cloned().unwrap_or(Value::Null);
            let ok = exit_code.as_i64() == Some(0);
            let output = read.get("text").cloned().unwrap_or_else(|| json!(""));
            let mut result = Map::new();
            result.insert("ok".to_owned(), json!(ok));
            result.insert("backend".to_owned(), json!("utility_pane"));
            result.insert("workspace".to_owned(), json!(workspace_id));
            result.insert("pane_id".to_owned(), json!(pane_id));
            result.insert("created_utility_pane".to_owned(), json!(created));
            result.insert("command".to_owned(), json!(command));
            result.insert("session_id".to_owned(), json!(session_id));
            result.insert("op_id".to_owned(), json!(session_id));
            result.insert("phase".to_owned(), json!("completed"));
            result.insert("exit_code".to_owned(), exit_code);
            result.insert("output".to_owned(), output);
            result.insert(
                "effective_cwd".to_owned(),
                json!(effective_root.to_string_lossy()),
            );
            result.insert(
                "project_root".to_owned(),
                json!(effective_root.to_string_lossy()),
            );
            if let Some(progress) = read.get("progress") {
                result.insert("progress".to_owned(), progress.clone());
            }
            if let Some(truncated) = read.get("truncated") {
                result.insert("truncated".to_owned(), truncated.clone());
            }
            if let Some(compacted) = read.get("compacted") {
                result.insert("compacted".to_owned(), compacted.clone());
            }
            if let Some(counts) = read.get("counts") {
                result.insert("counts".to_owned(), counts.clone());
            }
            let observed_exit_code = result.get("exit_code").and_then(Value::as_i64);
            exec_evidence::insert_completed_evidence(&mut result, observed_exit_code);
            project_structured_output(&mut result, registry, session_id);
            add_working_warning(&mut result, working);
            return Value::Object(result);
        }
        if started.elapsed() >= deadline {
            let partial = read.get("text").cloned().unwrap_or_else(|| json!(""));
            let mut result = Map::new();
            result.insert("ok".to_owned(), json!(false));
            result.insert("code".to_owned(), json!("exec_timeout"));
            result.insert("backend".to_owned(), json!("utility_pane"));
            result.insert("workspace".to_owned(), json!(workspace_id));
            result.insert("pane_id".to_owned(), json!(pane_id));
            result.insert("created_utility_pane".to_owned(), json!(created));
            result.insert("command".to_owned(), json!(command));
            result.insert("session_id".to_owned(), json!(session_id));
            result.insert("op_id".to_owned(), json!(session_id));
            result.insert("phase".to_owned(), json!("running"));
            result.insert("partial_output".to_owned(), partial);
            result.insert(
                "effective_cwd".to_owned(),
                json!(effective_root.to_string_lossy()),
            );
            result.insert(
                "project_root".to_owned(),
                json!(effective_root.to_string_lossy()),
            );
            if let Some(progress) = read.get("progress") {
                result.insert("progress".to_owned(), progress.clone());
            }
            exec_evidence::insert_timeout_evidence(&mut result);
            add_working_warning(&mut result, working);
            result.insert(
                "hint".to_owned(),
                json!("command is still tracked; call herdr_exec_read with this session_id for final status/output and do not re-send it"),
            );
            return Value::Object(result);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Native-default synchronous execution: start one durable native session, then
/// wait a bounded time for it. A single structured step uses literal argv via
/// `start_native_program`; a multi-step sequence reuses the shell-quoted
/// `&&` chain so it stays sequential, stops on the first non-zero exit, and
/// shares one bounded timeout. No second executor, queue, or scheduler is
/// introduced.
fn run_native_durable(
    registry: &ExecRegistry,
    target: (&str, &Path),
    invocation: &ExecInvocation,
    command: &str,
    timeout_ms: u64,
    working: &[Value],
) -> Value {
    let (workspace_id, effective_root) = target;
    let started = Instant::now();
    let started_result = match invocation {
        ExecInvocation::Command(command) => registry.start_native(effective_root, command),
        ExecInvocation::Steps(steps) => match steps.as_slice() {
            [step] => registry.start_native_program(effective_root, &step.program, &step.args),
            _ => registry.start_native(effective_root, &invocation.rendered()),
        },
    };
    let start = match started_result {
        Ok(value) => value,
        Err(message) => {
            let mut result = Map::new();
            result.insert("ok".to_owned(), json!(false));
            result.insert("code".to_owned(), json!("exec_start_failed"));
            result.insert("message".to_owned(), json!(message));
            result.insert("backend".to_owned(), json!("native"));
            result.insert("workspace".to_owned(), json!(workspace_id));
            result.insert("command".to_owned(), json!(command));
            result.insert("delivery_state".to_owned(), json!("unknown"));
            exec_evidence::insert_uncertain_start(&mut result);
            result.insert(
                "hint".to_owned(),
                json!("Native command start outcome is uncertain; existing session/process state determines whether another start is safe."),
            );
            return Value::Object(result);
        }
    };
    let Some(session_id) = start.get("session_id").and_then(Value::as_str) else {
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(false));
        result.insert("code".to_owned(), json!("exec_start_failed"));
        result.insert(
            "message".to_owned(),
            json!("native exec start returned no session_id"),
        );
        result.insert("backend".to_owned(), json!("native"));
        result.insert("workspace".to_owned(), json!(workspace_id));
        result.insert("command".to_owned(), json!(command));
        result.insert("delivery_state".to_owned(), json!("unknown"));
        exec_evidence::insert_uncertain_start(&mut result);
        return Value::Object(result);
    };
    let session_id = session_id.to_owned();
    let deadline = Duration::from_millis(timeout_ms);
    loop {
        let read = registry.read(&session_id, "both", 0, 65_536);
        if read.get("phase").and_then(Value::as_str) == Some("completed") {
            return native_completed_result(
                registry,
                workspace_id,
                effective_root,
                command,
                &session_id,
                &read,
                working,
            );
        }
        if started.elapsed() >= deadline {
            return native_timeout_result(
                workspace_id,
                effective_root,
                command,
                &session_id,
                &read,
                working,
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn native_completed_result(
    registry: &ExecRegistry,
    workspace_id: &str,
    effective_root: &Path,
    command: &str,
    session_id: &str,
    read: &Value,
    working: &[Value],
) -> Value {
    let exit_code = read.get("exit_code").cloned().unwrap_or(Value::Null);
    let ok = exit_code.as_i64() == Some(0);
    let output = read.get("text").cloned().unwrap_or_else(|| json!(""));
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(ok));
    result.insert("backend".to_owned(), json!("native"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("session_id".to_owned(), json!(session_id));
    result.insert("op_id".to_owned(), json!(session_id));
    result.insert("phase".to_owned(), json!("completed"));
    result.insert("exit_code".to_owned(), exit_code);
    result.insert("output".to_owned(), output);
    result.insert(
        "effective_cwd".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    result.insert(
        "project_root".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    for key in ["progress", "truncated", "compacted", "counts", "signal"] {
        if let Some(value) = read.get(key) {
            result.insert(key.to_owned(), value.clone());
        }
    }
    let observed_exit_code = result.get("exit_code").and_then(Value::as_i64);
    exec_evidence::insert_completed_evidence(&mut result, observed_exit_code);
    project_structured_output(&mut result, registry, session_id);
    add_working_warning(&mut result, working);
    Value::Object(result)
}

fn native_timeout_result(
    workspace_id: &str,
    effective_root: &Path,
    command: &str,
    session_id: &str,
    read: &Value,
    working: &[Value],
) -> Value {
    let partial = read.get("text").cloned().unwrap_or_else(|| json!(""));
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("exec_timeout"));
    result.insert("backend".to_owned(), json!("native"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("session_id".to_owned(), json!(session_id));
    result.insert("op_id".to_owned(), json!(session_id));
    result.insert("phase".to_owned(), json!("running"));
    result.insert("partial_output".to_owned(), partial);
    result.insert(
        "effective_cwd".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    result.insert(
        "project_root".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    if let Some(progress) = read.get("progress") {
        result.insert("progress".to_owned(), progress.clone());
    }
    exec_evidence::insert_timeout_evidence(&mut result);
    add_working_warning(&mut result, working);
    result.insert(
        "hint".to_owned(),
        json!("command is still tracked; call herdr_exec_read with this session_id for final status/output and do not re-send it"),
    );
    Value::Object(result)
}

/// Complete bounded content of one stream, or `None` when completeness cannot
/// be proven (missing session, dropped bytes, read-window truncation, or an
/// already compacted view). `structured_output` is only projected from proven-
/// complete content.
fn complete_stream_text(registry: &ExecRegistry, session_id: &str, stream: &str) -> Option<String> {
    let view = registry.read(
        session_id,
        stream,
        0,
        exec_evidence::STRUCTURED_OUTPUT_MAX_BYTES,
    );
    if view.get("ok").and_then(Value::as_bool) != Some(true)
        || view.get("truncated").and_then(Value::as_bool) == Some(true)
        || view.get("compacted").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let text = view.get("text").and_then(Value::as_str)?;
    let bytes_total = view.get("bytes_total").and_then(Value::as_u64)?;
    (bytes_total == text.len() as u64).then(|| text.to_owned())
}

/// Project a conservative `structured_output` from the stdout/stderr streams
/// (stdout preferred) while leaving the original `output` untouched.
fn project_structured_output(
    result: &mut Map<String, Value>,
    registry: &ExecRegistry,
    session_id: &str,
) {
    let stdout = complete_stream_text(registry, session_id, "stdout").unwrap_or_default();
    let stderr = complete_stream_text(registry, session_id, "stderr").unwrap_or_default();
    exec_evidence::insert_structured_output(result, &stdout, &stderr);
}

/// Utility-pane start failure. The stage is owned by `exec_sessions`, not
/// guessed from the message: a before-send failure proves the command was never
/// delivered, while an at/after-send failure must not claim that.
fn utility_pane_start_failure(
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    error: crate::exec_sessions::PaneStartError,
) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("exec_start_failed"));
    result.insert("message".to_owned(), json!(error.message));
    result.insert("backend".to_owned(), json!("utility_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("pane_id".to_owned(), json!(pane_id));
    result.insert("command".to_owned(), json!(command));
    if error.stage.proves_nothing_delivered() {
        // Preparation failed before the launch line was sent: nothing ran.
        result.insert("delivery_state".to_owned(), json!("not_delivered"));
        result.insert(
            "hint".to_owned(),
            json!("The utility-pane command was not delivered; the pane is unchanged."),
        );
        exec_evidence::insert_control_plane_rejection(&mut result);
    } else {
        // `send_text` may already have reached the pane, so this must never
        // claim `execution.started=false`.
        result.insert("delivery_state".to_owned(), json!("unknown"));
        result.insert(
            "hint".to_owned(),
            json!("Command start outcome is uncertain; existing pane/process state determines whether another start is safe."),
        );
        exec_evidence::insert_uncertain_start(&mut result);
    }
    Value::Object(result)
}

fn private_pane_start_failure(
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    error: crate::exec_sessions::PaneStartError,
) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("exec_start_failed"));
    result.insert("message".to_owned(), json!(error.message));
    result.insert("backend".to_owned(), json!("private_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("pane_id".to_owned(), json!(pane_id));
    result.insert("command".to_owned(), json!(command));
    if error.stage.proves_nothing_delivered() {
        result.insert("delivery_state".to_owned(), json!("not_delivered"));
        exec_evidence::insert_control_plane_rejection(&mut result);
    } else {
        result.insert("delivery_state".to_owned(), json!("unknown"));
        exec_evidence::insert_uncertain_start(&mut result);
    }
    Value::Object(result)
}

fn private_pane_control_plane_error(workspace_id: &str, command: &str, message: String) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("private_pane_unavailable"));
    result.insert("message".to_owned(), json!(message));
    result.insert("backend".to_owned(), json!("private_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("delivery_state".to_owned(), json!("not_delivered"));
    result.insert(
        "safe_retry_mode".to_owned(),
        json!("retry_after_control_plane_recovery"),
    );
    exec_evidence::insert_control_plane_rejection(&mut result);
    Value::Object(result)
}

fn utility_pane_control_plane_error(workspace_id: &str, command: &str, message: String) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("utility_pane_unavailable"));
    result.insert("message".to_owned(), json!(message));
    result.insert("backend".to_owned(), json!("utility_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("delivery_state".to_owned(), json!("not_delivered"));
    result.insert(
        "safe_retry_mode".to_owned(),
        json!("retry_after_control_plane_recovery"),
    );
    result.insert(
        "hint".to_owned(),
        json!("failed to prepare canonical utility pane before command delivery"),
    );
    exec_evidence::insert_control_plane_rejection(&mut result);
    Value::Object(result)
}

fn utility_pane_contention_result(
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    readiness: &PaneReadiness,
) -> Value {
    let mut result = json!({
        "ok": false,
        "code": "utility_pane_not_ready",
        "backend": "utility_pane",
        "workspace": workspace_id,
        "pane_id": pane_id,
        "command": command,
        "foreground": readiness.foreground,
        "shell_pid": readiness.shell_pid,
        "foreground_process_group_id": readiness.foreground_process_group_id,
        "conflict_key": format!("workspace:{workspace_id}:utility_pane"),
        "retryable": true,
        "retry_after_ms": 500,
        "delivery_state": "not_delivered",
        "safe_retry_mode": "retry_after_resource_release",
        "hint": "The utility pane is occupied by an interactive program (for example less/git/gh) and becomes available when that program releases it.",
    });
    if let Some(object) = result.as_object_mut() {
        exec_evidence::insert_control_plane_rejection(object);
    }
    result
}

#[cfg(unix)]
fn ensure_utility_pane_ready(
    client: &HerdrClient,
    registry: &ExecRegistry,
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    wait_budget: Duration,
) -> Result<(), Value> {
    let info = client
        .call_with_timeout(
            "pane.process_info",
            json!({"pane_id": pane_id}),
            PRE_SEND_TIMEOUT,
        )
        .map_err(|error| utility_pane_control_plane_error(workspace_id, command, error.message))?;
    let readiness = utility_pane_readiness(&info);
    let owner_session_id = registry.running_pane_session_id(pane_id);
    wait_for_owned_utility_pane(
        client,
        registry,
        (workspace_id, pane_id),
        command,
        readiness,
        owner_session_id.as_deref(),
        wait_budget,
    )
}

#[cfg(unix)]
fn wait_for_owned_utility_pane(
    client: &HerdrClient,
    registry: &ExecRegistry,
    target: (&str, &str),
    command: &str,
    mut readiness: PaneReadiness,
    owner_session_id: Option<&str>,
    wait_budget: Duration,
) -> Result<(), Value> {
    let (workspace_id, pane_id) = target;
    let Some(owner_session_id) = owner_session_id else {
        if readiness.ready {
            return Ok(());
        }
        return Err(utility_pane_contention_result(
            workspace_id,
            pane_id,
            command,
            &readiness,
        ));
    };

    let started = Instant::now();
    loop {
        // Pane readiness alone is not enough while this registry still owns a
        // live session. pane.send_text can acknowledge before the shell has
        // visibly handed foreground to the launched command; allowing another
        // send in that window would duplicate delivery into the same pane.
        if registry.running_pane_session_id(pane_id).is_none() && readiness.ready {
            return Ok(());
        }
        if started.elapsed() >= wait_budget {
            break;
        }
        let remaining = wait_budget.saturating_sub(started.elapsed());
        thread::sleep(UTILITY_OWNED_POLL.min(remaining));
        let info = client
            .call_with_timeout(
                "pane.process_info",
                json!({"pane_id": pane_id}),
                PRE_SEND_TIMEOUT,
            )
            .map_err(|error| {
                utility_pane_control_plane_error(workspace_id, command, error.message)
            })?;
        readiness = utility_pane_readiness(&info);
    }

    let mut result = utility_pane_contention_result(workspace_id, pane_id, command, &readiness);
    if let Some(object) = result.as_object_mut() {
        object.insert("owner".to_owned(), json!("herdr_exec_session"));
        object.insert("owner_session_id".to_owned(), json!(owner_session_id));
        object.insert(
            "waited_ms".to_owned(),
            json!(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        object.insert(
            "hint".to_owned(),
            json!("The canonical utility pane is still occupied by a Herdr-owned exec session after the bounded wait; observe that session before retrying."),
        );
    }
    Err(result)
}

fn resolve_workspace(snapshot: &Value, target: &str) -> Option<WorkspaceRecord> {
    snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|workspace| {
            let id = workspace
                .get("workspace_id")
                .and_then(Value::as_str)
                .or_else(|| workspace.get("id").and_then(Value::as_str))?;
            let label = workspace.get("label").and_then(Value::as_str);
            (id == target || label == Some(target)).then(|| WorkspaceRecord { id: id.to_owned() })
        })
}

fn project_view(project: &projects::ProjectInfo) -> Value {
    json!({
        "root": project.root.to_string_lossy(),
        "pane_ids": project.pane_ids,
        "dirty": project.dirty,
        "changed_files": project.changed_files,
        "vcs": project.vcs,
        "managed": project.managed,
    })
}

fn detailed_project_views(snapshot: &Value, workspace_id: &str) -> Vec<Value> {
    let topology = projects::derive(snapshot);
    projects::projects_for_workspace(&topology, workspace_id)
        .iter()
        .map(project_view)
        .collect()
}

fn select_project_root(
    requested: Option<&str>,
    roots: &[PathBuf],
) -> Result<Option<PathBuf>, PathBuf> {
    if let Some(requested) = requested {
        let wanted = absolute_path(Path::new(requested));
        if let Some(root) = roots.iter().find(|root| paths_equivalent(root, &wanted)) {
            return Ok(Some(root.clone()));
        }
        return Err(wanted);
    }
    match roots {
        [single] => Ok(Some(single.clone())),
        _ => Ok(None),
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if crate::macos_permissions::is_protected_user_path(left)
        || crate::macos_permissions::is_protected_user_path(right)
    {
        // Both candidates come from the live Herdr snapshot / explicit tool
        // argument. Do not canonicalize protected folders in the rotating
        // runtime merely to compare two project-root identities.
        return left == right;
    }
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

#[cfg(unix)]
fn prepare_utility_pane(
    client: &HerdrClient,
    snapshot: &Value,
    workspace_id: &str,
    cwd: &Path,
) -> Result<(String, bool), PrepareError> {
    let cached = panes_from_snapshot(snapshot, workspace_id);
    let remembered = utility_pane_id(workspace_id);
    if let Some(pane) = choose_utility_pane(&cached, remembered.as_deref()) {
        remember_utility_pane(workspace_id, &pane.id);
        return Ok((pane.id.clone(), false));
    }

    let mut last_taskgroup = None;
    for attempt in 0..3 {
        let panes = match fresh_panes(client, workspace_id) {
            Ok(panes) => panes,
            Err(error) if is_control_plane_taskgroup(&error.message) => {
                last_taskgroup = Some(error.message);
                thread::sleep(Duration::from_millis(100 + attempt * 200));
                continue;
            }
            Err(error) => {
                return Err(PrepareError::Other {
                    code: error.code,
                    message: error.message,
                });
            }
        };
        if let Some(pane) = choose_utility_pane(&panes, remembered.as_deref()) {
            remember_utility_pane(workspace_id, &pane.id);
            return Ok((pane.id.clone(), false));
        }
        forget_utility_pane(workspace_id, remembered.as_deref());
        let seed = panes
            .first()
            .or_else(|| cached.first())
            .map(|pane| pane.id.as_str());
        match split_utility_pane(client, workspace_id, seed, cwd) {
            Ok(pane_id) => {
                remember_utility_pane(workspace_id, &pane_id);
                return Ok((pane_id, true));
            }
            Err(error) if is_control_plane_taskgroup(&error.message) => {
                last_taskgroup = Some(error.message);
                thread::sleep(Duration::from_millis(100 + attempt * 200));
            }
            Err(error) => {
                return Err(PrepareError::Other {
                    code: error.code,
                    message: error.message,
                });
            }
        }
    }
    Err(PrepareError::ControlPlane(last_taskgroup.unwrap_or_else(
        || "utility pane unavailable before send".to_owned(),
    )))
}

fn choose_utility_pane<'a>(
    panes: &'a [PaneRecord],
    remembered: Option<&str>,
) -> Option<&'a PaneRecord> {
    remembered
        .and_then(|pane_id| panes.iter().find(|pane| pane.id == pane_id))
        .or_else(|| {
            panes
                .iter()
                .find(|pane| pane.label.as_deref() == Some(UTILITY_LABEL))
        })
}

fn utility_pane_id(workspace_id: &str) -> Option<String> {
    UTILITY_PANE_IDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(workspace_id)
        .cloned()
}

fn remember_utility_pane(workspace_id: &str, pane_id: &str) {
    UTILITY_PANE_IDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(workspace_id.to_owned(), pane_id.to_owned());
}

fn forget_utility_pane(workspace_id: &str, expected: Option<&str>) {
    let mut cache = UTILITY_PANE_IDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if expected.is_none() || cache.get(workspace_id).map(String::as_str) == expected {
        cache.remove(workspace_id);
    }
}

#[cfg(unix)]
fn split_utility_pane(
    client: &HerdrClient,
    workspace_id: &str,
    seed: Option<&str>,
    cwd: &Path,
) -> Result<String, HerdrError> {
    let mut params = Map::new();
    params.insert("direction".to_owned(), json!("right"));
    params.insert("cwd".to_owned(), json!(cwd.to_string_lossy()));
    params.insert("focus".to_owned(), json!(false));
    if let Some(seed) = seed {
        params.insert("target_pane_id".to_owned(), json!(seed));
    } else {
        params.insert("workspace_id".to_owned(), json!(workspace_id));
    }
    let result = client.call_with_timeout("pane.split", Value::Object(params), SPLIT_TIMEOUT)?;
    let pane_id = extract_pane_id(&result).ok_or_else(|| HerdrError {
        code: "pane_split_failed".to_owned(),
        message: "pane.split returned no pane id".to_owned(),
    })?;
    let _ = client.call_with_timeout(
        "pane.rename",
        json!({"pane_id": pane_id, "label": UTILITY_LABEL}),
        PRE_SEND_TIMEOUT,
    );
    let _ = client.call_with_timeout(
        "pane.wait_for_output",
        json!({
            "pane_id": pane_id,
            "source": "recent_unwrapped",
            "match": {"type": "regex", "value": "[%#$>❯] ?$"},
            "timeout_ms": 5_000,
        }),
        Duration::from_secs(6),
    );
    thread::sleep(Duration::from_millis(300));
    Ok(pane_id)
}

fn panes_from_snapshot(snapshot: &Value, workspace_id: &str) -> Vec<PaneRecord> {
    snapshot
        .get("panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|pane| pane.get("workspace_id").and_then(Value::as_str) == Some(workspace_id))
        .filter_map(pane_record)
        .collect()
}

fn fresh_panes(client: &HerdrClient, workspace_id: &str) -> Result<Vec<PaneRecord>, HerdrError> {
    let result = client.call_with_timeout(
        "pane.list",
        json!({"workspace_id": workspace_id}),
        PRE_SEND_TIMEOUT,
    )?;
    let panes = result
        .get("panes")
        .and_then(Value::as_array)
        .or_else(|| result.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(panes.iter().filter_map(pane_record).collect())
}

fn pane_record(value: &Value) -> Option<PaneRecord> {
    Some(PaneRecord {
        id: value
            .get("pane_id")
            .and_then(Value::as_str)
            .or_else(|| value.get("id").and_then(Value::as_str))?
            .to_owned(),
        label: value
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn extract_pane_id(value: &Value) -> Option<String> {
    let pane = value.get("pane").unwrap_or(value);
    pane.get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| pane.get("id").and_then(Value::as_str))
        .map(str::to_owned)
}

fn utility_pane_readiness(raw: &Value) -> PaneReadiness {
    let info = raw.get("process_info").unwrap_or(raw);
    let shell_pid = finite_pid(info.get("shell_pid"));
    let foreground_process_group_id = finite_pid(info.get("foreground_process_group_id"));
    let foreground = info
        .get("foreground_processes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let shell_owns_foreground = shell_pid.is_some()
        && foreground_process_group_id.is_some()
        && shell_pid == foreground_process_group_id;
    let only_shell_foreground = !foreground.is_empty()
        && foreground
            .iter()
            .all(|process| finite_pid(process.get("pid")) == shell_pid);
    PaneReadiness {
        ready: shell_owns_foreground && only_shell_foreground,
        shell_pid,
        foreground_process_group_id,
        foreground,
    }
}

fn finite_pid(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            value
                .and_then(Value::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0)
        })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn cleanup_stale_scripts() -> usize {
    let base = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let cutoff = SystemTime::now()
        .checked_sub(STALE_SCRIPT_AGE)
        .unwrap_or(UNIX_EPOCH);
    let Ok(entries) = fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("herdr-mcp-exec-") || !name.ends_with(".sh") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified < cutoff && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn is_control_plane_taskgroup(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("exceptiongroup")
        || lower.contains("unhandled errors in a taskgroup")
        || (lower.contains("taskgroup")
            && (lower.contains("unhandled") || lower.contains("sub-exception")))
}

fn add_working_warning(result: &mut Map<String, Value>, working: &[Value]) {
    if !working.is_empty() {
        result.insert("warnings".to_owned(), json!({"working": working}));
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(invalid(&format!("{key} must be a string"))),
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(invalid(&format!("{key} must be a boolean"))),
    }
}

fn optional_u64(args: &Value, key: &str, min: u64, max: u64) -> Result<Option<u64>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value
            .as_u64()
            .filter(|value| *value >= min && *value <= max)
        {
            Some(value) => Ok(Some(value)),
            None => Err(invalid(&format!(
                "{key} must be an integer in {min}..={max}"
            ))),
        },
    }
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    #[test]
    fn project_root_selection_is_fail_closed() {
        let roots = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        assert_eq!(select_project_root(None, &roots).unwrap(), None);
        assert_eq!(
            select_project_root(Some("/tmp/a"), &roots).unwrap(),
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(
            select_project_root(Some("/tmp/c"), &roots).unwrap_err(),
            PathBuf::from("/tmp/c")
        );
    }

    #[test]
    fn structured_exec_compiles_literal_argv_into_one_local_sequence() {
        let invocation = resolve_exec_invocation(&json!({
            "workspace": "w1",
            "steps": [
                {"program": "printf", "args": ["%s\\n", "a; b", "$(uname)", "it's literal"]},
                {"program": "/usr/bin/git", "args": ["status", "--short"]}
            ]
        }))
        .unwrap();
        assert!(matches!(invocation, ExecInvocation::Steps(ref steps) if steps.len() == 2));
        assert_eq!(
            invocation.rendered(),
            "'printf' '%s\\n' 'a; b' '$(uname)' 'it'\\''s literal' && '/usr/bin/git' 'status' '--short'"
        );
    }

    #[test]
    fn structured_exec_rejects_mixed_modes_and_unknown_step_fields() {
        let mixed = resolve_exec_invocation(&json!({
            "command": "git status",
            "steps": [{"program": "git", "args": ["status"]}]
        }))
        .unwrap_err();
        assert_eq!(mixed["code"], "invalid_params");

        let unknown = resolve_exec_invocation(&json!({
            "steps": [{"program": "git", "shell": true}]
        }))
        .unwrap_err();
        assert_eq!(unknown["code"], "invalid_params");
    }

    #[test]
    fn structured_exec_single_step_prefers_native_program_rendering() {
        let invocation = resolve_exec_invocation(&json!({
            "steps": [{"program": "printf", "args": ["%s", "a b"]}]
        }))
        .unwrap();
        match &invocation {
            ExecInvocation::Steps(steps) => assert_eq!(steps.len(), 1),
            ExecInvocation::Command(_) => panic!("expected structured steps"),
        }
        assert_eq!(invocation.rendered(), "'printf' '%s' 'a b'");
    }

    #[cfg(unix)]
    #[test]
    fn private_pane_session_never_uses_canonical_utility_and_cancel_reclaims_it() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = env::temp_dir().join(format!(
            "herdr-private-exec-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        ));
        fs::create_dir_all(&base).unwrap();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let methods = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_methods = Arc::clone(&methods);
        let server = thread::spawn(move || {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let request: Value = serde_json::from_str(
                    &BufReader::new(stream.try_clone().unwrap())
                        .lines()
                        .next()
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                server_methods.lock().unwrap().push(method.clone());
                let result = match method.as_str() {
                    "pane.split" => json!({"pane": {"pane_id": "w1:p9"}}),
                    "pane.rename" | "pane.wait_for_output" | "pane.send_text" | "pane.close" => {
                        json!({"ok": true})
                    }
                    other => panic!("unexpected Herdr method: {other}"),
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"id": request["id"].clone(), "result": result}),
                )
                .unwrap();
            }
        });

        let client = HerdrClient::new(&socket);
        let registry =
            ExecRegistry::new_with_client(base.join("state"), Some(client.clone())).unwrap();
        let result =
            start_private_pane_session(&client, &registry, "w1", Path::new("/tmp"), "sleep 30");
        assert_eq!(result["ok"], true);
        assert_eq!(result["backend"], "private_pane");
        assert_eq!(result["pane_id"], "w1:p9");
        assert_eq!(result["created_exec_pane"], true);

        let session_id = result["session_id"].as_str().unwrap();
        assert_eq!(registry.kill(session_id)["ok"], true);
        server.join().unwrap();
        assert_eq!(
            *methods.lock().unwrap(),
            vec![
                "pane.split",
                "pane.rename",
                "pane.wait_for_output",
                "pane.send_text",
                "pane.close",
            ],
        );
        drop(registry);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn remembered_utility_pane_survives_label_propagation_delay() {
        let panes = vec![
            PaneRecord {
                id: "w1:p1".to_owned(),
                label: None,
            },
            PaneRecord {
                id: "w1:p2".to_owned(),
                label: None,
            },
        ];
        let selected = choose_utility_pane(&panes, Some("w1:p2")).unwrap();
        assert_eq!(selected.id, "w1:p2");
    }

    #[test]
    fn readiness_requires_shell_to_own_foreground() {
        let ready = utility_pane_readiness(&json!({
            "process_info": {
                "shell_pid": 42,
                "foreground_process_group_id": 42,
                "foreground_processes": [{"pid": 42, "name": "zsh"}]
            }
        }));
        assert!(ready.ready);
        let blocked = utility_pane_readiness(&json!({
            "process_info": {
                "shell_pid": 42,
                "foreground_process_group_id": 77,
                "foreground_processes": [{"pid": 77, "name": "less"}]
            }
        }));
        assert!(!blocked.ready);
    }

    #[cfg(unix)]
    #[test]
    fn herdr_owned_busy_utility_pane_waits_for_shell_before_send() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = native_test_dir();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "pane.process_info");
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"].clone(),
                    "result": {
                        "process_info": {
                            "shell_pid": 42,
                            "foreground_process_group_id": 42,
                            "foreground_processes": [{"pid": 42, "name": "zsh"}],
                        }
                    }
                })
            )
            .unwrap();
        });
        let client = HerdrClient::new(&socket);
        let registry = ExecRegistry::new(base.join("state")).unwrap();
        let busy = PaneReadiness {
            ready: false,
            shell_pid: Some(42),
            foreground_process_group_id: Some(77),
            foreground: vec![json!({"pid": 77, "name": "sleep"})],
        };

        let result = wait_for_owned_utility_pane(
            &client,
            &registry,
            ("w1", "w1:p2"),
            "git status",
            busy,
            Some("es_owned"),
            Duration::from_millis(250),
        );
        assert!(result.is_ok(), "owned pane should become ready: {result:?}");
        server.join().unwrap();
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn live_owner_blocks_second_send_even_when_shell_still_looks_ready() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = native_test_dir();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let methods = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_methods = Arc::clone(&methods);
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let request: Value = serde_json::from_str(
                    &BufReader::new(stream.try_clone().unwrap())
                        .lines()
                        .next()
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                server_methods.lock().unwrap().push(method.clone());
                let result = match method.as_str() {
                    "pane.send_text" | "pane.send_keys" => json!({"ok": true}),
                    "pane.process_info" => json!({
                        "process_info": {
                            "shell_pid": 42,
                            "foreground_process_group_id": 42,
                            "foreground_processes": [{"pid": 42, "name": "zsh"}],
                        }
                    }),
                    other => panic!("unexpected Herdr method: {other}"),
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"id": request["id"].clone(), "result": result}),
                )
                .unwrap();
            }
        });

        let client = HerdrClient::new(&socket);
        let registry =
            ExecRegistry::new_with_client(base.join("state"), Some(client.clone())).unwrap();
        let started = registry
            .start_in_existing_pane(Path::new("/tmp"), "sleep 30", "w1:p2")
            .unwrap();
        let owner = started["session_id"].as_str().unwrap().to_owned();
        let shell_ready = PaneReadiness {
            ready: true,
            shell_pid: Some(42),
            foreground_process_group_id: Some(42),
            foreground: vec![json!({"pid": 42, "name": "zsh"})],
        };

        let blocked = wait_for_owned_utility_pane(
            &client,
            &registry,
            ("w1", "w1:p2"),
            "git status",
            shell_ready,
            Some(&owner),
            Duration::from_millis(60),
        )
        .unwrap_err();
        assert_eq!(blocked["code"], "utility_pane_not_ready");
        assert_eq!(blocked["delivery_state"], "not_delivered");
        assert_eq!(blocked["owner_session_id"], owner);

        assert_eq!(registry.kill(&owner)["ok"], true);
        server.join().unwrap();
        assert_eq!(
            *methods.lock().unwrap(),
            vec!["pane.send_text", "pane.process_info", "pane.send_keys"],
        );
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn foreign_busy_utility_pane_never_enters_owned_wait() {
        let base = native_test_dir();
        let registry = ExecRegistry::new(base.join("state")).unwrap();
        let client = HerdrClient::new(Path::new("/definitely/not/a/socket"));
        let busy = PaneReadiness {
            ready: false,
            shell_pid: Some(42),
            foreground_process_group_id: Some(77),
            foreground: vec![json!({"pid": 77, "name": "less"})],
        };
        let started = Instant::now();
        let result = wait_for_owned_utility_pane(
            &client,
            &registry,
            ("w1", "w1:p2"),
            "git status",
            busy,
            None,
            Duration::from_millis(250),
        )
        .unwrap_err();
        assert_eq!(result["code"], "utility_pane_not_ready");
        assert!(result.get("owner_session_id").is_none());
        assert!(started.elapsed() < Duration::from_millis(100));
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn utility_pane_control_plane_failure_never_falls_back_locally() {
        let result = utility_pane_control_plane_error(
            "w1",
            "git status",
            "control plane unavailable".to_owned(),
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "utility_pane_unavailable");
        assert_eq!(result["backend"], "utility_pane");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(
            result["safe_retry_mode"],
            "retry_after_control_plane_recovery"
        );
        assert_eq!(
            result["execution"],
            json!({"started": false, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], "herdr_control_plane");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_durable_protected_documents_root_uses_utility_path_on_control_plane_failure() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = native_test_dir();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                writeln!(
                    stream,
                    "{}",
                    json!({
                        "id": request["id"].clone(),
                        "error": {
                            "code": "snapshot_error",
                            "message": "unhandled errors in a TaskGroup (1 sub-exception)"
                        }
                    })
                )
                .unwrap();
            }
        });
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"));
        let root = home.join("Documents").join(format!(
            "herdr-protected-routing-test-{}",
            std::process::id()
        ));
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(&socket);
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "printf 'must-not-run-natively\\n'"}),
        );

        assert_eq!(result["ok"], false, "unexpected result: {result}");
        assert_eq!(result["code"], "utility_pane_unavailable");
        assert_eq!(result["backend"], "utility_pane");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["execution"]["started"], false);
        assert_eq!(result["failure_origin"], "herdr_control_plane");
        assert!(registry.list_views().is_empty());

        server.join().unwrap();
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn utility_pane_contention_is_explicitly_retryable_before_delivery() {
        let readiness = PaneReadiness {
            ready: false,
            shell_pid: Some(42),
            foreground_process_group_id: Some(77),
            foreground: vec![json!({"pid": 77, "name": "less"})],
        };
        let result = utility_pane_contention_result("w1", "w1:p2", "git status", &readiness);
        assert_eq!(result["code"], "utility_pane_not_ready");
        assert_eq!(result["conflict_key"], "workspace:w1:utility_pane");
        assert_eq!(result["retryable"], true);
        assert_eq!(result["retry_after_ms"], 500);
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["safe_retry_mode"], "retry_after_resource_release");
        // Machine-decidable pre-start semantics: nothing ran, and the failure
        // belongs to the Herdr control plane rather than to a child process.
        assert_eq!(
            result["execution"],
            json!({"started": false, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], "herdr_control_plane");
        assert!(result.get("structured_output").is_none());
    }

    #[test]
    fn utility_pane_start_failure_stage_owns_the_delivery_claim() {
        use crate::exec_sessions::PaneStartError;

        // Before-send preparation failure: the command never reached the pane.
        let before = utility_pane_start_failure(
            "w1",
            "w1:p2",
            "lark-cli --json",
            PaneStartError::before_send("Herdr pane backend is unavailable"),
        );
        assert_eq!(before["code"], "exec_start_failed");
        assert_eq!(before["delivery_state"], "not_delivered");
        assert_eq!(
            before["execution"],
            json!({"started": false, "completed": false, "exit_code": null})
        );
        assert_eq!(before["failure_origin"], "herdr_control_plane");

        // At/after-send failure: delivery cannot be ruled out.
        let after = utility_pane_start_failure(
            "w1",
            "w1:p2",
            "lark-cli --json",
            PaneStartError::after_send("cannot start utility pane command: send failed"),
        );
        assert_eq!(after["code"], "exec_start_failed");
        assert_eq!(after["delivery_state"], "unknown");
        assert_eq!(after["execution"]["started"], Value::Null);
        assert_ne!(after["execution"]["started"], json!(false));
        assert_eq!(after["failure_origin"], "unknown");
    }

    #[test]
    fn pane_start_stage_is_owned_by_the_exec_backend_not_guessed_from_text() {
        use crate::exec_sessions::{PaneStartError, PaneStartStage};

        assert!(
            PaneStartError::before_send("cannot write pane script")
                .stage
                .proves_nothing_delivered()
        );
        assert!(
            !PaneStartError::after_send("cannot write pane script")
                .stage
                .proves_nothing_delivered()
        );
        assert_eq!(
            PaneStartStage::BeforeSend,
            PaneStartError::before_send("x").stage
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_durable_protected_root_reports_pre_start_evidence_on_prepare_failure() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = native_test_dir();
        let socket = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..1 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                writeln!(
                    stream,
                    "{}",
                    json!({
                        "id": request["id"].clone(),
                        "error": {
                            "code": "pane_list_failed",
                            "message": "cannot list panes in this workspace"
                        }
                    })
                )
                .unwrap();
            }
        });
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"));
        let root = home.join("Documents").join(format!(
            "herdr-protected-prepare-failure-test-{}",
            std::process::id()
        ));
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(&socket);
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "printf 'must-not-run-natively\n'"}),
        );

        assert_eq!(result["ok"], false, "unexpected result: {result}");
        assert_eq!(result["code"], "pane_list_failed");
        assert_eq!(result["backend"], "utility_pane");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(
            result["execution"],
            json!({"started": false, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], "herdr_control_plane");
        assert!(registry.list_views().is_empty());

        server.join().unwrap();
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn taskgroup_detection_is_narrow() {
        assert!(is_control_plane_taskgroup(
            "unhandled errors in a TaskGroup (1 sub-exception)"
        ));
        assert!(is_control_plane_taskgroup("ExceptionGroup: boom"));
        assert!(!is_control_plane_taskgroup("ordinary command timeout"));
    }

    #[test]
    fn workspace_resolution_accepts_id_or_label() {
        let snapshot = json!({"workspaces": [{"workspace_id": "w1", "label": "demo"}]});
        assert_eq!(resolve_workspace(&snapshot, "w1").unwrap().id, "w1");
        assert_eq!(resolve_workspace(&snapshot, "demo").unwrap().id, "w1");
        assert!(resolve_workspace(&snapshot, "missing").is_none());
    }

    fn native_test_dir() -> PathBuf {
        static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let base = env::temp_dir().join(format!(
            "herdr-native-exec-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            sequence,
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    /// In-memory durable store for exec durability tests.
    fn test_store() -> Arc<Mutex<StateStore>> {
        Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()))
    }

    fn native_test_snapshot(root: &Path) -> Value {
        json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {"checkout_path": root.to_string_lossy()},
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy(),
            }],
        })
    }

    #[test]
    fn project_root_mismatch_reports_pre_start_control_plane_evidence() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();
        let outside = base.join("outside");

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({
                "workspace": "w1",
                "project_root": outside.to_string_lossy(),
                "steps": [{"program": "printf", "args": ["must-not-run"]}],
            }),
        );

        assert_eq!(result["ok"], false);
        assert_eq!(result["reason"], "project_root_not_in_workspace");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["execution"]["started"], false);
        assert_eq!(result["execution"]["completed"], false);
        assert!(result["execution"]["exit_code"].is_null());
        assert_eq!(result["failure_origin"], "herdr_control_plane");
        assert!(registry.list_views().is_empty());

        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn native_default_succeeds_without_a_pane_socket() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        // Point the client at a socket that does not exist: the default native
        // path must not need the Herdr control plane at all.
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "printf 'native-default-ok\\n'"}),
        );

        assert_eq!(result["ok"], true, "unexpected result: {result}");
        assert_eq!(result["backend"], "native");
        assert_eq!(result["phase"], "completed");
        assert_eq!(result["exit_code"], 0);
        assert!(result["session_id"].as_str().is_some());
        assert_eq!(result["op_id"], result["session_id"]);
        assert!(result.get("pane_id").is_none());
        assert!(result.get("created_utility_pane").is_none());
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("native-default-ok")
        );
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn native_default_timeout_keeps_the_same_session_and_never_restarts() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "sleep 30", "timeout_ms": 250}),
        );

        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "exec_timeout");
        assert_eq!(result["backend"], "native");
        assert_eq!(result["phase"], "running");
        let session_id = result["session_id"]
            .as_str()
            .expect("timeout keeps session_id");
        assert!(result["hint"].as_str().unwrap().contains("do not re-send"));

        // The timed-out command is still the single tracked session; the
        // client can resume it instead of starting a second execution.
        let views = registry.list_views();
        assert_eq!(views.len(), 1, "unexpected sessions: {views:?}");
        assert_eq!(views[0]["session_id"], session_id);
        let read = registry.read(session_id, "both", 0, 65_536);
        assert_eq!(read["ok"], true);
        assert_eq!(read["phase"], "running");
        assert_eq!(registry.kill(session_id)["killed"], true);
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    /// Regression for the real 1.0 failure: a child CLI exits 2 while printing
    /// exactly one JSON object. The result must prove the child process ran and
    /// ended, attribute the failure to it instead of to the Herdr control
    /// plane, and project the payload without dropping the raw output.
    #[cfg(unix)]
    #[test]
    fn native_child_cli_json_error_exit_two_is_child_process_evidence() {
        use std::os::unix::fs::PermissionsExt;

        const PAYLOAD: &str = r#"{"ok":false,"error":{"type":"validation","subtype":"invalid_argument","message":"flag needs an argument: --json"}}"#;

        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let cli = base.join("fake-lark-cli.sh");
        fs::write(
            &cli,
            format!("#!/bin/sh\nprintf '%s\\n' '{PAYLOAD}'\nexit 2\n"),
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();

        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({
                "workspace": "w1",
                "steps": [{"program": cli.to_string_lossy(), "args": ["--json"]}]
            }),
        );

        assert_eq!(result["ok"], false, "unexpected result: {result}");
        assert_eq!(result["backend"], "native");
        assert_eq!(result["phase"], "completed");
        assert_eq!(result["exit_code"], 2);
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": true, "exit_code": 2})
        );
        assert_eq!(result["failure_origin"], "child_process");
        // No pre-delivery claim may leak into an executed child result.
        assert!(result.get("delivery_state").is_none());
        // The original output is preserved verbatim…
        let output = result["output"].as_str().expect("raw output kept");
        assert!(
            output.contains("flag needs an argument: --json"),
            "{output}"
        );
        // …and the payload is also projected conservatively.
        assert_eq!(
            result["structured_output"],
            serde_json::from_str::<serde_json::Value>(PAYLOAD).unwrap()
        );
        assert_eq!(result["structured_output"]["ok"], false);
        assert_eq!(
            result["structured_output"]["error"]["subtype"],
            "invalid_argument"
        );
        assert_eq!(result["structured_output_stream"], "stdout");

        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn native_successful_json_object_is_projected_without_failure_origin() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "printf '{\"task\":\"ok\"}'"}),
        );

        assert_eq!(result["ok"], true, "unexpected result: {result}");
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": true, "exit_code": 0})
        );
        assert!(result.get("failure_origin").is_none());
        assert_eq!(result["structured_output"], json!({"task": "ok"}));

        // Non-object stdout stays unprojected.
        let array = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "printf '[1,2,3]'"}),
        );
        assert_eq!(array["ok"], true, "unexpected result: {array}");
        assert!(array.get("structured_output").is_none());

        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn native_timeout_is_not_reported_as_a_child_failure() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({"workspace": "w1", "command": "sleep 30", "timeout_ms": 250}),
        );

        assert_eq!(result["code"], "exec_timeout");
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], "timeout");
        assert_ne!(result["failure_origin"], "child_process");
        assert_ne!(result["failure_origin"], "herdr_control_plane");
        assert!(result.get("structured_output").is_none());

        let session_id = result["session_id"].as_str().unwrap();
        assert_eq!(registry.kill(session_id)["killed"], true);
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn native_structured_steps_stop_on_first_non_zero_exit() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();

        let result = run_durable(
            &client,
            &snapshot,
            &registry,
            &test_store(),
            &json!({
                "workspace": "w1",
                "steps": [
                    {"program": "printf", "args": ["first-ok"]},
                    {"program": "/bin/sh", "args": ["-c", "exit 7"]},
                    {"program": "printf", "args": ["never-runs"]}
                ]
            }),
        );

        assert_eq!(result["ok"], false, "unexpected result: {result}");
        assert_eq!(result["backend"], "native");
        assert_eq!(result["exit_code"], 7);
        let output = result["output"].as_str().unwrap();
        assert!(
            output.contains("first-ok"),
            "missing first step output: {output}"
        );
        assert!(
            !output.contains("never-runs"),
            "steps must stop on first failure: {output}"
        );
        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    /// A keyed `non_idempotent_write` must deliver the side effect exactly once
    /// and replay the recorded synchronous result — same session_id, same
    /// output — without a second delivery.
    #[cfg(unix)]
    #[test]
    fn keyed_write_executes_once_and_replays_the_recorded_result() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("side-effect.txt");
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();
        let store = test_store();
        let command = format!("printf x >> {}", marker.to_string_lossy());
        let args = json!({
            "workspace": "w1",
            "command": command,
            "intent": "non_idempotent_write",
            "idempotency_key": "side-effect-once",
        });

        let first = run_durable(&client, &snapshot, &registry, &store, &args);
        assert_eq!(first["ok"], true, "unexpected result: {first}");
        assert_eq!(first["phase"], "completed");
        assert_eq!(first["intent"], "non_idempotent_write");
        assert_eq!(first["idempotency_persisted"], true);
        assert_eq!(first["idempotent_replay"], false);
        assert_eq!(first["safe_retry_mode"], "none");
        let first_session = first["session_id"].clone();
        assert_eq!(fs::read_to_string(&marker).unwrap(), "x");

        let replay = run_durable(&client, &snapshot, &registry, &store, &args);
        assert_eq!(replay["ok"], true, "unexpected replay: {replay}");
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["idempotency_persisted"], true);
        assert_eq!(replay["intent"], "non_idempotent_write");
        assert_eq!(replay["session_id"], first_session);
        assert_eq!(replay["output"], first["output"]);
        // The replay preserves the original synchronous result identity; it is
        // never rewritten with the durable ledger's own op_id.
        assert_eq!(replay["op_id"], first["op_id"]);
        // The single side effect proves no second delivery happened.
        assert_eq!(fs::read_to_string(&marker).unwrap(), "x");

        // A completed key must replay identically even when an agent in the
        // same project became busy after the original execution. The replay
        // delivers no command, so it must never be turned into a live
        // agent_working rejection.
        let mut busy_snapshot = snapshot.clone();
        busy_snapshot["agents"] = json!([{
            "agent": "pi",
            "pane_id": "w1:p1",
            "workspace_id": "w1",
            "cwd": root.to_string_lossy(),
            "agent_status": "working",
        }]);
        let busy_replay = run_durable(&client, &busy_snapshot, &registry, &store, &args);
        assert_eq!(
            busy_replay["ok"], true,
            "unexpected busy replay: {busy_replay}"
        );
        assert_eq!(busy_replay["idempotent_replay"], true);
        assert_eq!(busy_replay["idempotency_persisted"], true);
        assert_eq!(busy_replay["session_id"], first_session);
        assert_eq!(busy_replay["op_id"], first["op_id"]);
        assert_eq!(busy_replay["output"], first["output"]);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "x");

        // A brand-new keyed request is still gated by the busy mutation check
        // before any delivery. The rejected reservation is released, so the
        // same key succeeds exactly once after the agent settles instead of
        // staying blocked as an in-flight key.
        let gate_marker = root.join("gate-effect.txt");
        let gate_args = json!({
            "workspace": "w1",
            "command": format!("printf g >> {}", gate_marker.to_string_lossy()),
            "intent": "non_idempotent_write",
            "idempotency_key": "gate-key",
        });
        let busy_new = run_durable(&client, &busy_snapshot, &registry, &store, &gate_args);
        assert_eq!(busy_new["ok"], false, "unexpected busy gate: {busy_new}");
        assert_eq!(busy_new["reason"], "agent_working");
        assert_eq!(busy_new["delivery_state"], "not_delivered");
        assert!(!gate_marker.exists());
        let after_settle = run_durable(&client, &snapshot, &registry, &store, &gate_args);
        assert_eq!(
            after_settle["ok"], true,
            "unexpected settled run: {after_settle}"
        );
        assert_eq!(after_settle["idempotent_replay"], false);
        assert_eq!(fs::read_to_string(&gate_marker).unwrap(), "g");
        let settled_replay = run_durable(&client, &snapshot, &registry, &store, &gate_args);
        assert_eq!(settled_replay["idempotent_replay"], true);
        assert_eq!(fs::read_to_string(&gate_marker).unwrap(), "g");

        // Legacy calls are untouched: the same command without intent/key runs
        // again and reports no durability metadata at all.
        let legacy = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({"workspace": "w1", "command": command}),
        );
        assert_eq!(legacy["ok"], true, "unexpected legacy: {legacy}");
        assert!(legacy.get("intent").is_none());
        assert!(legacy.get("idempotent_replay").is_none());
        assert!(legacy.get("idempotency_persisted").is_none());
        assert_eq!(fs::read_to_string(&marker).unwrap(), "xx");

        // An explicit read_only without a key stays accepted, reserves nothing,
        // and reports bounded intent metadata without claiming a replay.
        let read_only = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({"workspace": "w1", "command": "printf ro", "intent": "read_only"}),
        );
        assert_eq!(read_only["ok"], true, "unexpected read_only: {read_only}");
        assert_eq!(read_only["intent"], "read_only");
        assert_eq!(read_only["idempotency_persisted"], false);
        assert_eq!(read_only["idempotent_replay"], false);

        drop(registry);
        let _ = fs::remove_dir_all(base);
    }

    /// Reusing a settled key for a different request must fail closed with no
    /// execution, so a conflict can never replay or overwrite another request.
    #[cfg(unix)]
    #[test]
    fn keyed_request_reuse_with_a_different_command_conflicts_without_executing() {
        let base = native_test_dir();
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("conflict-effect.txt");
        let snapshot = native_test_snapshot(&root);
        let client = HerdrClient::new(base.join("missing-herdr.sock"));
        let registry = ExecRegistry::new(base.join("state")).unwrap();
        let store = test_store();

        let first = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({
                "workspace": "w1",
                "steps": [{"program": "printf", "args": ["first"]}],
                "intent": "idempotent_write",
                "idempotency_key": "bound-key",
            }),
        );
        assert_eq!(first["ok"], true, "unexpected first result: {first}");
        assert_eq!(first["idempotent_replay"], false);

        // Same key, different execution mode that renders identically: the
        // structured argv and the freeform command both print "first", but the
        // fingerprint binds the mode, so this is a conflict.
        let conflicting_step = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({
                "workspace": "w1",
                "command": "printf first",
                "intent": "idempotent_write",
                "idempotency_key": "bound-key",
            }),
        );
        assert_eq!(conflicting_step["ok"], false);
        assert_eq!(conflicting_step["code"], "idempotency_key_conflict");
        assert_eq!(conflicting_step["delivery_state"], "not_delivered");
        assert_eq!(conflicting_step["execution"]["started"], false);
        assert!(!marker.exists());

        // Same key, genuinely different command: also refused, and nothing runs.
        let conflicting = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({
                "workspace": "w1",
                "command": format!("printf y > {}", marker.to_string_lossy()),
                "intent": "idempotent_write",
                "idempotency_key": "bound-key",
            }),
        );
        assert_eq!(conflicting["ok"], false);
        assert_eq!(conflicting["code"], "idempotency_key_conflict");
        assert_eq!(conflicting["delivery_state"], "not_delivered");
        assert_eq!(conflicting["execution"]["started"], false);
        assert!(!marker.exists());

        // A key without an intent, and a write intent without a key, are
        // rejected before execution as invalid parameters.
        let keyless_intent = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({"workspace": "w1", "command": "printf no", "idempotency_key": "k"}),
        );
        assert_eq!(keyless_intent["code"], "invalid_params");
        let intentless_key = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({"workspace": "w1", "command": "printf no", "intent": "idempotent_write"}),
        );
        assert_eq!(intentless_key["code"], "invalid_params");
        assert!(!marker.exists());

        // A reservation whose outcome is not yet settled must fail closed. The
        // pending row is the delivery guard: the same key must not run again,
        // and the command's side effect must never be produced.
        let pending_command = format!("printf pending > {}", marker.to_string_lossy());
        let pending_fingerprint = exec_request_fingerprint(
            "w1",
            &root,
            &ExecInvocation::Command(pending_command.clone()),
            Some(ExecIntent::NonIdempotentWrite),
            false,
            DEFAULT_TIMEOUT_MS,
        );
        store
            .lock()
            .unwrap()
            .reserve_operation(
                EXEC_OPERATION_KIND,
                &exec_key_hash("pending-key"),
                &pending_fingerprint,
                "op:exec:pending",
                1,
                None,
            )
            .unwrap();
        let in_flight = run_durable(
            &client,
            &snapshot,
            &registry,
            &store,
            &json!({
                "workspace": "w1",
                "command": pending_command,
                "intent": "non_idempotent_write",
                "idempotency_key": "pending-key",
            }),
        );
        assert_eq!(in_flight["ok"], false, "unexpected in-flight: {in_flight}");
        assert_eq!(in_flight["code"], "idempotency_in_flight");
        assert_eq!(in_flight["delivery_state"], "unknown");
        assert!(in_flight["execution"]["started"].is_null());
        assert_eq!(
            in_flight["failure_origin"],
            exec_evidence::FAILURE_ORIGIN_UNKNOWN
        );
        assert!(
            !marker.exists(),
            "a pending reservation must never deliver the command"
        );

        drop(registry);
        let _ = fs::remove_dir_all(base);
    }
}
