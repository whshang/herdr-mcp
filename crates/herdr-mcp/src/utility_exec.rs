use crate::exec_evidence;
use crate::exec_sessions::ExecRegistry;
use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::projects;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const UTILITY_LABEL: &str = "herdr-mcp:utility";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const PRE_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_SCRIPT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STRUCTURED_STEPS: usize = 16;
const MAX_STRUCTURED_ARGS: usize = 128;
static UTILITY_PREPARE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
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

pub fn run_durable(
    client: &HerdrClient,
    snapshot: &Value,
    registry: &ExecRegistry,
    args: &Value,
) -> Value {
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
        return json!({
            "ok": false,
            "reason": "workspace_not_found",
            "workspace": workspace_target,
        });
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
            return json!({
                "ok": false,
                "reason": "project_root_required",
                "workspace": workspace.id,
                "candidates": [],
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "This workspace has no current project root; project_root requires a current attached project.",
            });
        }
        Ok(None) => {
            return json!({
                "ok": false,
                "reason": "project_root_required",
                "workspace": workspace.id,
                "candidates": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "This workspace has multiple project roots; project_root must match one of the returned candidates.",
            });
        }
        Err(wanted) => {
            return json!({
                "ok": false,
                "reason": "project_root_not_in_workspace",
                "workspace": workspace.id,
                "project_root": wanted.to_string_lossy(),
                "candidates": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "project_root must match one of this workspace's returned project-root candidates.",
            });
        }
    };
    let working =
        match mutation::check_with_topology(snapshot, &topology, &effective_root, confirm_busy) {
            Ok(working) => working,
            Err(error) => return error,
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
            return run_unix_durable(
                client,
                snapshot,
                registry,
                (&workspace.id, &effective_root),
                &command,
                timeout_ms,
                &working,
            );
        }
        #[cfg(not(unix))]
        {
            return json!({
                "ok": false,
                "code": "unsupported_platform",
                "message": "protected-path execution requires the macOS utility-pane transport",
                "workspace": workspace.id,
                "command": command,
                "effective_cwd": effective_root.to_string_lossy(),
                "project_root": effective_root.to_string_lossy(),
            });
        }
    }

    #[cfg(not(unix))]
    let _ = (client, snapshot);

    run_native_durable(
        registry,
        (&workspace.id, &effective_root),
        &invocation,
        &command,
        timeout_ms,
        &working,
    )
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

pub(crate) fn start_reusable_pane_session(
    client: &HerdrClient,
    snapshot: &Value,
    registry: &ExecRegistry,
    workspace_id: &str,
    effective_root: &Path,
    command: &str,
) -> Value {
    #[cfg(windows)]
    {
        let _ = (
            client,
            snapshot,
            registry,
            workspace_id,
            effective_root,
            command,
        );
        return json!({
            "ok": false,
            "code": "unsupported_platform",
            "message": "reusable utility-pane execution requires the Windows Herdr named-pipe transport, which is still pending",
        });
    }

    #[cfg(unix)]
    {
        let (pane_id, created) =
            match prepare_utility_pane(client, snapshot, workspace_id, effective_root) {
                Ok(value) => value,
                Err(PrepareError::ControlPlane(message)) => {
                    return utility_pane_control_plane_error(workspace_id, command, message);
                }
                Err(PrepareError::Other { code, message }) => {
                    return json!({
                        "ok": false,
                        "code": code,
                        "message": message,
                        "workspace": workspace_id,
                        "command": command,
                        "delivery_state": "not_delivered",
                        "hint": "failed to prepare canonical utility pane before command delivery",
                    });
                }
            };

        if let Ok(info) = client.call_with_timeout(
            "pane.process_info",
            json!({"pane_id": pane_id}),
            PRE_SEND_TIMEOUT,
        ) {
            let readiness = utility_pane_readiness(&info);
            if !readiness.ready {
                return utility_pane_contention_result(workspace_id, &pane_id, command, &readiness);
            }
        }

        let mut result = match registry.start_in_existing_pane(effective_root, command, &pane_id) {
            Ok(value) => value,
            Err(message) => {
                return utility_pane_start_failure(workspace_id, &pane_id, command, message);
            }
        };
        if let Some(object) = result.as_object_mut() {
            object.insert("workspace".to_owned(), json!(workspace_id));
            object.insert("created_utility_pane".to_owned(), json!(created));
        }
        result
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
    let (pane_id, created) =
        match prepare_utility_pane(client, snapshot, workspace_id, effective_root) {
            Ok(value) => value,
            Err(PrepareError::ControlPlane(message)) => {
                return utility_pane_control_plane_error(workspace_id, command, message);
            }
            Err(PrepareError::Other { code, message }) => {
                return json!({
                    "ok": false,
                    "code": code,
                    "message": message,
                    "workspace": workspace_id,
                    "command": command,
                    "hint": "failed to prepare utility pane before command delivery",
                });
            }
        };

    if let Ok(info) = client.call_with_timeout(
        "pane.process_info",
        json!({"pane_id": pane_id}),
        PRE_SEND_TIMEOUT,
    ) {
        let readiness = utility_pane_readiness(&info);
        if !readiness.ready {
            return utility_pane_contention_result(workspace_id, &pane_id, command, &readiness);
        }
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

fn utility_pane_start_failure(
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    message: String,
) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert("code".to_owned(), json!("exec_start_failed"));
    result.insert("message".to_owned(), json!(message));
    result.insert("backend".to_owned(), json!("utility_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("pane_id".to_owned(), json!(pane_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("delivery_state".to_owned(), json!("unknown"));
    result.insert(
        "hint".to_owned(),
        json!("Command start outcome is uncertain; existing pane/process state determines whether another start is safe."),
    );
    // `send_text` may already have reached the pane, so this must never claim
    // `execution.started=false`.
    exec_evidence::insert_uncertain_start(&mut result);
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
    let _guard = UTILITY_PREPARE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    fn reusable_pane_session_reuses_canonical_pane_and_cancel_preserves_it() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let base = env::temp_dir().join(format!(
            "herdr-utility-reuse-{}-{}",
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
                    "pane.process_info" => json!({
                        "process_info": {
                            "shell_pid": 42,
                            "foreground_process_group_id": 42,
                            "foreground_processes": [{"pid": 42, "name": "zsh"}],
                        }
                    }),
                    "pane.send_text" => json!({"ok": true}),
                    "pane.send_keys" => {
                        assert_eq!(request["params"]["keys"], json!(["C-c"]));
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
        let snapshot = json!({
            "panes": [{
                "workspace_id": "w1",
                "pane_id": "w1:p2",
                "label": UTILITY_LABEL,
            }]
        });
        let result = start_reusable_pane_session(
            &client,
            &snapshot,
            &registry,
            "w1",
            Path::new("/tmp"),
            "sleep 30",
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["backend"], "utility_pane");
        assert_eq!(result["pane_id"], "w1:p2");
        assert_eq!(result["created_utility_pane"], false);

        let session_id = result["session_id"].as_str().unwrap();
        let killed = registry.kill(session_id);
        assert_eq!(killed["ok"], true);
        server.join().unwrap();
        assert_eq!(
            *methods.lock().unwrap(),
            vec!["pane.process_info", "pane.send_text", "pane.send_keys"],
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
    fn utility_pane_start_failure_never_claims_nothing_ran() {
        let result = utility_pane_start_failure(
            "w1",
            "w1:p2",
            "lark-cli --json",
            "cannot start utility pane command".to_owned(),
        );
        assert_eq!(result["code"], "exec_start_failed");
        assert_eq!(result["delivery_state"], "unknown");
        assert_eq!(result["execution"]["started"], Value::Null);
        assert_ne!(result["execution"]["started"], json!(false));
        assert_eq!(result["failure_origin"], "unknown");
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
}
