use crate::exec_compact;
use crate::exec_sessions::{enriched_exec_path, resolve_exec_shell};
use crate::herdr::{HerdrClient, HerdrError};
use crate::mutation;
use crate::projects;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const UTILITY_LABEL: &str = "herdr-mcp:utility";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const PRE_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const SPLIT_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 8_000;
const PARTIAL_OUTPUT_LIMIT: usize = 4_000;
const STALE_SCRIPT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
static NEXT_EXEC: AtomicU64 = AtomicU64::new(0);
static UTILITY_PREPARE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static UTILITY_PANE_IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
type LocalChunks = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

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

#[derive(Debug)]
struct LocalResult {
    exit_code: Option<i32>,
    signal: Option<String>,
    output: String,
    timed_out: bool,
    truncated: bool,
}

pub fn run(client: &HerdrClient, snapshot: &Value, args: &Value) -> Value {
    let workspace_target = match required_str(args, "workspace") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let command = match required_str(args, "command") {
        Ok("") => return invalid("command must not be empty"),
        Ok(value) => value,
        Err(error) => return error,
    };
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
                "hint": "workspace has no current project root — create or attach a project, then re-call with project_root set to the returned root",
            });
        }
        Ok(None) => {
            return json!({
                "ok": false,
                "reason": "project_root_required",
                "workspace": workspace.id,
                "candidates": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "current_projects": detailed_project_views(snapshot, &workspace.id),
                "hint": "workspace has multiple project roots — re-call with project_root set to one of candidates",
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
                "hint": "project_root must be one of this workspace's current project roots — re-call with project_root set to one of candidates",
            });
        }
    };

    let working =
        match mutation::check_with_topology(snapshot, &topology, &effective_root, confirm_busy) {
            Ok(working) => working,
            Err(error) => return error,
        };
    let _ = cleanup_stale_scripts();

    #[cfg(windows)]
    {
        let _ = client;
        let _ = timeout_ms;
        return json!({
            "ok": false,
            "code": "unsupported_platform",
            "message": "visible utility-pane execution requires the Windows Herdr named-pipe transport, which is still pending",
            "workspace": workspace.id,
            "command": command,
            "effective_cwd": effective_root.to_string_lossy(),
            "project_root": effective_root.to_string_lossy(),
        });
    }

    #[cfg(unix)]
    run_unix(
        client,
        snapshot,
        &workspace.id,
        &effective_root,
        command,
        timeout_ms,
        &working,
    )
}

#[cfg(unix)]
fn run_unix(
    client: &HerdrClient,
    snapshot: &Value,
    workspace_id: &str,
    effective_root: &Path,
    command: &str,
    timeout_ms: u64,
    working: &[Value],
) -> Value {
    let started_at_ms = now_ms();
    let (mut pane_id, mut created) =
        match prepare_utility_pane(client, snapshot, workspace_id, effective_root) {
            Ok(value) => value,
            Err(PrepareError::ControlPlane(message)) => {
                return local_fallback(
                    command,
                    effective_root,
                    workspace_id,
                    timeout_ms,
                    "control_plane_taskgroup_before_send",
                    working,
                    Some(message),
                );
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
            return json!({
                "ok": false,
                "code": "utility_pane_not_ready",
                "backend": "utility_pane",
                "workspace": workspace_id,
                "pane_id": pane_id,
                "command": command,
                "foreground": readiness.foreground,
                "shell_pid": readiness.shell_pid,
                "foreground_process_group_id": readiness.foreground_process_group_id,
                "hint": "utility pane is owned by an interactive program (for example less/git/gh); exit it and retry herdr_exec",
            });
        }
    }

    let sequence = NEXT_EXEC.fetch_add(1, Ordering::Relaxed);
    let exec_id = format!("utility-{}-{sequence}", std::process::id());
    let marker = format!("__HM_EXEC_RUST_{}_{}_EXIT_", std::process::id(), sequence);
    let exec_shell = resolve_exec_shell();
    let script_path = temp_script_path(sequence);
    let script_body = build_utility_exec_script(&exec_shell, effective_root, command, &exec_id);
    if let Err(error) = write_executable_script(&script_path, &script_body) {
        return json!({
            "ok": false,
            "reason": "script_write_failed",
            "message": error,
            "workspace": workspace_id,
            "pane_id": pane_id,
            "command": command,
        });
    }
    let cmdline = utility_launch_line(&exec_shell, &script_path, &marker);

    let send_result = client.call_with_timeout(
        "pane.send_text",
        json!({"pane_id": pane_id, "text": format!("{cmdline}\n")}),
        PRE_SEND_TIMEOUT,
    );
    if let Err(error) = send_result {
        if matches!(error.code.as_str(), "pane_not_found" | "unknown_pane") {
            match recover_utility_pane(client, snapshot, workspace_id, effective_root, &pane_id) {
                Ok((next_id, next_created)) => {
                    pane_id = next_id;
                    created |= next_created;
                    match client.call_with_timeout(
                        "pane.send_text",
                        json!({"pane_id": pane_id, "text": format!("{cmdline}\n")}),
                        PRE_SEND_TIMEOUT,
                    ) {
                        Ok(_) => {}
                        Err(second)
                            if matches!(
                                second.code.as_str(),
                                "pane_not_found" | "unknown_pane"
                            ) =>
                        {
                            let _ = fs::remove_file(&script_path);
                            return local_fallback(
                                command,
                                effective_root,
                                workspace_id,
                                timeout_ms,
                                &format!("pane_recover_failed:{}", second.code),
                                working,
                                None,
                            );
                        }
                        Err(second) => {
                            return post_send_uncertain(
                                workspace_id,
                                &pane_id,
                                command,
                                working,
                                &second,
                                "recovered pane send was not acknowledged; command may or may not have run — inspect the utility pane and do not blind-retry",
                            );
                        }
                    }
                }
                Err(PrepareError::ControlPlane(message)) => {
                    let _ = fs::remove_file(&script_path);
                    return local_fallback(
                        command,
                        effective_root,
                        workspace_id,
                        timeout_ms,
                        "control_plane_taskgroup_pane_recover",
                        working,
                        Some(message),
                    );
                }
                Err(PrepareError::Other { code, .. }) => {
                    let _ = fs::remove_file(&script_path);
                    return local_fallback(
                        command,
                        effective_root,
                        workspace_id,
                        timeout_ms,
                        &format!("pane_recover_failed:{code}"),
                        working,
                        None,
                    );
                }
            }
        } else {
            return post_send_uncertain(
                workspace_id,
                &pane_id,
                command,
                working,
                &error,
                if is_control_plane_taskgroup(&error.message) {
                    "pane.send_text hit a control-plane TaskGroup — command may or may not have run; inspect utility pane or herdr_since, do not re-send the same command"
                } else {
                    "pane.send_text did not return delivery confirmation — inspect the utility pane before retrying"
                },
            );
        }
    }

    let wait_timeout = Duration::from_millis(timeout_ms.saturating_add(10_000).min(MAX_TIMEOUT_MS));
    let wait_result = client.call_with_timeout(
        "pane.wait_for_output",
        json!({
            "pane_id": pane_id,
            "source": "recent_unwrapped",
            "match": {"type": "regex", "value": format!("{marker}\\d+__")},
            "timeout_ms": timeout_ms,
        }),
        wait_timeout,
    );
    if let Err(error) = wait_result {
        let partial = read_pane_text(client, &pane_id, 80)
            .map(|text| {
                let (_, segment) = extract_command_result(&text, &cmdline, &marker);
                tail_chars(clean_terminal_output(&segment).trim(), PARTIAL_OUTPUT_LIMIT)
            })
            .unwrap_or_default();
        let timed_out = error.code == "timeout";
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(false));
        result.insert(
            "code".to_owned(),
            json!(if timed_out {
                "exec_timeout"
            } else {
                error.code.as_str()
            }),
        );
        if !timed_out {
            result.insert(
                "message".to_owned(),
                json!(if is_control_plane_taskgroup(&error.message) {
                    unwrap_control_plane_message(&error.message)
                } else {
                    error.message
                }),
            );
        }
        result.insert("backend".to_owned(), json!("utility_pane"));
        result.insert("workspace".to_owned(), json!(workspace_id));
        result.insert("pane_id".to_owned(), json!(pane_id));
        result.insert("command".to_owned(), json!(command));
        result.insert(
            "effective_cwd".to_owned(),
            json!(effective_root.to_string_lossy()),
        );
        result.insert(
            "project_root".to_owned(),
            json!(effective_root.to_string_lossy()),
        );
        result.insert("partial_output".to_owned(), json!(partial));
        add_working_warning(&mut result, working);
        result.insert(
            "hint".to_owned(),
            json!(if timed_out {
                "command may still be running in the utility pane — inspect it via pane.read"
            } else {
                "wait_for_output failed after command delivery — inspect pane output and do not re-send the command"
            }),
        );
        return Value::Object(result);
    }

    let raw = match read_pane_text(client, &pane_id, 200) {
        Ok(value) => value,
        Err(error) => {
            let mut result = json!({
                "ok": false,
                "code": "pane_read_failed",
                "message": if is_control_plane_taskgroup(&error.message) {
                    unwrap_control_plane_message(&error.message)
                } else {
                    error.message
                },
                "backend": "utility_pane",
                "workspace": workspace_id,
                "pane_id": pane_id,
                "command": command,
                "hint": "command was sent; use herdr_call pane.read on this pane_id — do not re-run herdr_exec with the same command",
            });
            if let Some(object) = result.as_object_mut() {
                add_working_warning(object, working);
            }
            return result;
        }
    };

    let (exit_code, segment) = extract_command_result(&raw, &cmdline, &marker);
    let cleaned = clean_terminal_output(&segment);
    let trimmed = cleaned.trim();
    let truncated = trimmed.chars().count() > OUTPUT_LIMIT;
    let output = tail_chars(trimmed, OUTPUT_LIMIT);
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(exit_code == Some(0)));
    result.insert("backend".to_owned(), json!("utility_pane"));
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("pane_id".to_owned(), json!(pane_id));
    result.insert("created_utility_pane".to_owned(), json!(created));
    result.insert("command".to_owned(), json!(command));
    result.insert("exit_code".to_owned(), json!(exit_code));
    result.insert(
        "effective_cwd".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    result.insert(
        "project_root".to_owned(),
        json!(effective_root.to_string_lossy()),
    );
    result.insert("truncated".to_owned(), json!(truncated));
    exec_compact::insert_compacted_or_raw(
        &mut result,
        "output",
        &output,
        exit_code == Some(0) && !truncated,
    );
    insert_sync_completion(&mut result, started_at_ms, output.len());
    add_working_warning(&mut result, working);
    Value::Object(result)
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

#[cfg(unix)]
fn recover_utility_pane(
    client: &HerdrClient,
    snapshot: &Value,
    workspace_id: &str,
    cwd: &Path,
    stale_id: &str,
) -> Result<(String, bool), PrepareError> {
    let _guard = UTILITY_PREPARE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    forget_utility_pane(workspace_id, Some(stale_id));
    let panes = fresh_panes(client, workspace_id).map_err(|error| {
        if is_control_plane_taskgroup(&error.message) {
            PrepareError::ControlPlane(error.message)
        } else {
            PrepareError::Other {
                code: error.code,
                message: error.message,
            }
        }
    })?;
    if let Some(pane) = panes
        .iter()
        .find(|pane| pane.id != stale_id && pane.label.as_deref() == Some(UTILITY_LABEL))
    {
        remember_utility_pane(workspace_id, &pane.id);
        return Ok((pane.id.clone(), false));
    }
    let cached = panes_from_snapshot(snapshot, workspace_id);
    let seed = panes
        .iter()
        .find(|pane| pane.id != stale_id)
        .or_else(|| panes.first())
        .map(|pane| pane.id.clone())
        .or_else(|| {
            cached
                .iter()
                .find(|pane| pane.id != stale_id)
                .map(|pane| pane.id.clone())
        });
    split_utility_pane(client, workspace_id, seed.as_deref(), cwd)
        .map(|pane| {
            remember_utility_pane(workspace_id, &pane);
            (pane, true)
        })
        .map_err(|error| {
            if is_control_plane_taskgroup(&error.message) {
                PrepareError::ControlPlane(error.message)
            } else {
                PrepareError::Other {
                    code: error.code,
                    message: error.message,
                }
            }
        })
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

#[cfg(unix)]
fn build_utility_exec_script(
    exec_shell: &Path,
    cwd: &Path,
    command: &str,
    exec_id: &str,
) -> String {
    [
        format!("#!{}", exec_shell.display()),
        "set +e".to_owned(),
        // Visible utility-pane commands are Herdr-managed executions just like
        // native exec sessions. Preserve that identity in the child process so
        // service/dev lifecycle guards cannot be bypassed by running a mutation
        // through herdr_exec and severing the control path that submitted it.
        format!("export HERDR_MCP_EXEC_ID={}", shell_quote(exec_id)),
        "export PAGER=cat".to_owned(),
        "export GIT_PAGER=cat".to_owned(),
        "export GH_PAGER=cat".to_owned(),
        "export SYSTEMD_PAGER=cat".to_owned(),
        "export MANPAGER=cat".to_owned(),
        "export DELTA_PAGER=cat".to_owned(),
        "trap 'rm -f -- \"$0\"' EXIT".to_owned(),
        format!(
            "cd -- {} || exit 127",
            shell_quote(cwd.to_string_lossy().as_ref())
        ),
        command.to_owned(),
    ]
    .join("\n")
        + "\n"
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn utility_launch_line(exec_shell: &Path, script_path: &Path, marker: &str) -> String {
    format!(
        "{} {}; ec=$?; rm -f -- {}; printf '\\n{}%s__' \"$ec\"",
        shell_quote(exec_shell.to_string_lossy().as_ref()),
        shell_quote(script_path.to_string_lossy().as_ref()),
        shell_quote(script_path.to_string_lossy().as_ref()),
        marker,
    )
}

fn temp_script_path(sequence: u64) -> PathBuf {
    let base = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    base.join(format!(
        "herdr-mcp-exec-{}-{sequence}.sh",
        std::process::id()
    ))
}

#[cfg(unix)]
fn write_executable_script(path: &Path, body: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)
        .map_err(|error| format!("cannot create utility script: {error}"))?;
    file.write_all(body.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot write utility script: {error}"))?;
    file.set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure utility script: {error}"))?;
    Ok(())
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

fn read_pane_text(client: &HerdrClient, pane_id: &str, lines: u64) -> Result<String, HerdrError> {
    let result = client.call_with_timeout(
        "pane.read",
        json!({
            "pane_id": pane_id,
            "source": "recent_unwrapped",
            "lines": lines,
            "strip_ansi": true,
        }),
        PRE_SEND_TIMEOUT,
    )?;
    let read = result.get("read").unwrap_or(&result);
    Ok(read
        .get("content")
        .and_then(Value::as_str)
        .or_else(|| read.get("text").and_then(Value::as_str))
        .or_else(|| read.get("output").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned())
}

fn extract_command_result(raw: &str, cmdline: &str, marker: &str) -> (Option<i32>, String) {
    let segment_start = raw.rfind(cmdline).map_or(0, |index| index + cmdline.len());
    let after_echo = &raw[segment_start..];
    let marker_index = after_echo.find(marker).unwrap_or(after_echo.len());
    let segment = after_echo[..marker_index].to_owned();
    let exit_code = raw.match_indices(marker).find_map(|(index, _)| {
        let suffix = &raw[index + marker.len()..];
        let digits = suffix
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        (!digits.is_empty() && suffix[digits.len()..].starts_with("__"))
            .then(|| digits.parse::<i32>().ok())
            .flatten()
    });
    (exit_code, segment)
}

fn clean_terminal_output(text: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let ansi =
        ANSI.get_or_init(|| Regex::new(r"\x1b\[\??[0-9;]*[A-Za-z]").expect("valid ansi regex"));
    let stripped = ansi.replace_all(text, "");
    stripped
        .lines()
        .filter(|line| !line.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

fn is_control_plane_taskgroup(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("exceptiongroup")
        || lower.contains("unhandled errors in a taskgroup")
        || (lower.contains("taskgroup")
            && (lower.contains("unhandled") || lower.contains("sub-exception")))
}

fn unwrap_control_plane_message(message: &str) -> String {
    let lines = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some(concrete) = lines.iter().find(|line| {
        let lower = line.to_ascii_lowercase();
        !lower.starts_with("exceptiongroup")
            && !lower.starts_with("unhandled errors in a taskgroup")
            && !lower.contains("sub-exception")
    }) {
        return (*concrete).to_owned();
    }
    "herdr control-plane TaskGroup blip (sub-exception not expanded by daemon)".to_owned()
}

#[cfg(unix)]
fn local_fallback(
    command: &str,
    cwd: &Path,
    workspace_id: &str,
    timeout_ms: u64,
    reason: &str,
    working: &[Value],
    detail: Option<String>,
) -> Value {
    let started_at_ms = now_ms();
    let local = run_local_shell(command, cwd, timeout_ms, OUTPUT_LIMIT);
    let mut result = Map::new();
    result.insert(
        "ok".to_owned(),
        json!(!local.timed_out && local.exit_code == Some(0)),
    );
    if local.timed_out {
        result.insert("code".to_owned(), json!("exec_timeout"));
    }
    result.insert("backend".to_owned(), json!("local_fallback"));
    result.insert("fallback_reason".to_owned(), json!(reason));
    if let Some(detail) = detail {
        result.insert(
            "fallback_detail".to_owned(),
            json!(unwrap_control_plane_message(&detail)),
        );
    }
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("exit_code".to_owned(), json!(local.exit_code));
    result.insert("signal".to_owned(), json!(local.signal));
    result.insert("effective_cwd".to_owned(), json!(cwd.to_string_lossy()));
    result.insert("project_root".to_owned(), json!(cwd.to_string_lossy()));
    result.insert("truncated".to_owned(), json!(local.truncated));
    exec_compact::insert_compacted_or_raw(
        &mut result,
        "output",
        &local.output,
        !local.timed_out && local.exit_code == Some(0) && !local.truncated,
    );
    insert_sync_completion(&mut result, started_at_ms, local.output.len());
    if local.timed_out {
        result.insert(
            "hint".to_owned(),
            json!("local fallback timed out; its isolated process group was terminated"),
        );
    }
    add_working_warning(&mut result, working);
    Value::Object(result)
}

#[cfg(unix)]
fn run_local_shell(command: &str, cwd: &Path, timeout_ms: u64, max_bytes: usize) -> LocalResult {
    let sequence = NEXT_EXEC.fetch_add(1, Ordering::Relaxed);
    let exec_id = format!("local-{}-{sequence}", std::process::id());
    let shell = resolve_exec_shell();
    let script_path = env::temp_dir().join(format!(
        "herdr-mcp-local-{}-{sequence}.sh",
        std::process::id()
    ));
    let body = build_utility_exec_script(&shell, cwd, command, &exec_id);
    if write_executable_script(&script_path, &body).is_err() {
        return LocalResult {
            exit_code: None,
            signal: None,
            output: String::new(),
            timed_out: false,
            truncated: false,
        };
    }

    let mut child = match Command::new(&shell)
        .arg(&script_path)
        .current_dir(cwd)
        .env("PATH", enriched_exec_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            let _ = fs::remove_file(&script_path);
            return LocalResult {
                exit_code: None,
                signal: None,
                output: String::new(),
                timed_out: false,
                truncated: false,
            };
        }
    };
    let pid = child.id();
    let chunks = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
    let sequence_counter = Arc::new(AtomicU64::new(0));
    let stdout_thread = child.stdout.take().map(|stdout| {
        spawn_local_reader(stdout, Arc::clone(&chunks), Arc::clone(&sequence_counter))
    });
    let stderr_thread = child.stderr.take().map(|stderr| {
        spawn_local_reader(stderr, Arc::clone(&chunks), Arc::clone(&sequence_counter))
    });
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
                thread::sleep(Duration::from_millis(800));
                if child.try_wait().ok().flatten().is_none() {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break child.wait().ok(),
        }
    };
    if let Some(handle) = stdout_thread {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    let _ = fs::remove_file(&script_path);
    let mut chunks = chunks
        .lock()
        .map(|chunks| chunks.clone())
        .unwrap_or_default();
    chunks.sort_by_key(|(seq, _)| *seq);
    let mut bytes = Vec::new();
    let mut truncated = false;
    for (_, chunk) in chunks {
        let room = max_bytes.saturating_sub(bytes.len());
        if room == 0 {
            truncated = true;
            break;
        }
        let take = chunk.len().min(room);
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            truncated = true;
            break;
        }
    }
    let mut output = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        output.push_str("\n…[truncated]");
    }
    LocalResult {
        exit_code: status.as_ref().and_then(ExitStatus::code),
        signal: status
            .as_ref()
            .and_then(|status| status.signal())
            .map(signal_name),
        output,
        timed_out,
        truncated,
    }
}

#[cfg(unix)]
fn spawn_local_reader<R: Read + Send + 'static>(
    mut reader: R,
    chunks: LocalChunks,
    sequence: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let seq = sequence.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut chunks) = chunks.lock() {
                chunks.push((seq, buffer[..read].to_vec()));
            }
        }
    })
}

#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGTERM => "SIGTERM".to_owned(),
        libc::SIGKILL => "SIGKILL".to_owned(),
        libc::SIGINT => "SIGINT".to_owned(),
        other => format!("SIG{other}"),
    }
}

fn post_send_uncertain(
    workspace_id: &str,
    pane_id: &str,
    command: &str,
    working: &[Value],
    error: &HerdrError,
    hint: &str,
) -> Value {
    let mut result = Map::new();
    result.insert("ok".to_owned(), json!(false));
    result.insert(
        "code".to_owned(),
        json!(if is_control_plane_taskgroup(&error.message) {
            "delivery_uncertain"
        } else {
            error.code.as_str()
        }),
    );
    result.insert("failure".to_owned(), json!("herdr_internal"));
    result.insert(
        "message".to_owned(),
        json!(if is_control_plane_taskgroup(&error.message) {
            unwrap_control_plane_message(&error.message)
        } else {
            error.message.clone()
        }),
    );
    result.insert("workspace".to_owned(), json!(workspace_id));
    result.insert("pane_id".to_owned(), json!(pane_id));
    result.insert("command".to_owned(), json!(command));
    result.insert("delivery".to_owned(), json!("uncertain"));
    result.insert("hint".to_owned(), json!(hint));
    add_working_warning(&mut result, working);
    Value::Object(result)
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn iso_from_ms(ms: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

fn insert_sync_completion(result: &mut Map<String, Value>, started_at_ms: u64, bytes_total: usize) {
    let elapsed_ms = now_ms().saturating_sub(started_at_ms);
    result.insert("started_at".to_owned(), json!(iso_from_ms(started_at_ms)));
    result.insert("phase".to_owned(), json!("completed"));
    result.insert(
        "progress".to_owned(),
        json!({
            "bytes_read": bytes_total,
            "bytes_total": bytes_total,
            "elapsed_ms": elapsed_ms,
        }),
    );
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn script_disables_pagers_quotes_cwd_and_self_cleans() {
        #[cfg(unix)]
        {
            let script = build_utility_exec_script(
                Path::new("/bin/sh"),
                Path::new("/tmp/a'b"),
                "git log -1",
                "utility-test-1",
            );
            assert!(script.contains("export HERDR_MCP_EXEC_ID='utility-test-1'"));
            assert!(script.contains("export GIT_PAGER=cat"));
            assert!(script.contains("export GH_PAGER=cat"));
            assert!(script.contains("trap 'rm -f -- \"$0\"' EXIT"));
            assert!(script.contains("cd -- '/tmp/a'\\''b' || exit 127"));
            assert!(script.ends_with("git log -1\n"));
        }
    }

    #[test]
    fn command_result_uses_last_echo_and_real_marker() {
        let marker = "MARK";
        let cmd = "'/bin/sh' '/tmp/x'; ec=$?; printf 'MARK%s__' \"$ec\"";
        let raw = format!("old\n{cmd}\nhello\n{marker}42__\nprompt");
        let (code, segment) = extract_command_result(&raw, cmd, marker);
        assert_eq!(code, Some(42));
        assert_eq!(segment.trim(), "hello");
    }

    #[test]
    fn partial_segment_never_includes_prior_pane_history() {
        let marker = "MARK";
        let cmd = "'/bin/sh' '/tmp/x'; ec=$?; printf 'MARK%s__' \"$ec\"";
        let raw = format!("SECRET_FROM_OLD_HISTORY\n{cmd}\ncurrent-only\n");
        let (_, segment) = extract_command_result(&raw, cmd, marker);
        assert_eq!(segment.trim(), "current-only");
        assert!(!segment.contains("SECRET_FROM_OLD_HISTORY"));
    }

    #[test]
    fn taskgroup_detection_is_narrow() {
        assert!(is_control_plane_taskgroup(
            "unhandled errors in a TaskGroup (1 sub-exception)"
        ));
        assert!(is_control_plane_taskgroup("ExceptionGroup: boom"));
        assert!(!is_control_plane_taskgroup("ordinary command timeout"));
    }

    #[cfg(unix)]
    #[test]
    fn local_fallback_marks_managed_execution() {
        let result = local_fallback(
            "printf '%s' \"$HERDR_MCP_EXEC_ID\"",
            Path::new("/tmp"),
            "w1",
            5_000,
            "test",
            &[],
            None,
        );
        assert_eq!(result["ok"], true);
        let output = result["output"].as_str().expect("fallback output");
        assert!(output.starts_with("local-"), "unexpected exec id: {output}");
    }

    #[cfg(unix)]
    #[test]
    fn local_fallback_compacts_large_success_only() {
        let large = local_fallback(
            "awk 'BEGIN{for(i=0;i<90;i++) print \"line-\" i}'",
            Path::new("/tmp"),
            "w1",
            5_000,
            "test",
            &[],
            None,
        );
        assert_eq!(large["ok"], true);
        assert_eq!(large["phase"], "completed");
        assert!(large["started_at"].as_str().is_some());
        assert_eq!(
            large["progress"]["bytes_read"],
            large["progress"]["bytes_total"]
        );
        assert!(large["progress"]["elapsed_ms"].as_u64().is_some());
        assert_eq!(large["exit_code"], 0);
        assert_eq!(
            large["command"],
            "awk 'BEGIN{for(i=0;i<90;i++) print \"line-\" i}'"
        );
        assert_eq!(large["effective_cwd"], "/tmp");
        assert_eq!(large["truncated"], false);
        assert_eq!(large["compacted"], true);
        assert_eq!(large["counts"]["lines"], 90);
        let output = large["output"].as_str().unwrap();
        assert!(output.contains("line-0\n"));
        assert!(output.contains("…[omitted 30 lines]…"));
        assert!(!output.contains("line-40\n"));

        let small = local_fallback(
            "printf 'hello-local\\n'",
            Path::new("/tmp"),
            "w1",
            5_000,
            "test",
            &[],
            None,
        );
        assert_eq!(small["ok"], true);
        assert!(small["output"].as_str().unwrap().contains("hello-local"));
        assert!(small.get("compacted").is_none());

        let failed = local_fallback(
            "awk 'BEGIN{for(i=0;i<90;i++) print \"fail-\" i}'; exit 3",
            Path::new("/tmp"),
            "w1",
            5_000,
            "test",
            &[],
            None,
        );
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["exit_code"], 3);
        assert!(failed.get("compacted").is_none());
        let fail_output = failed["output"].as_str().unwrap();
        assert!(fail_output.contains("fail-0\n"));
        assert!(fail_output.contains("fail-40\n"));
        assert!(fail_output.contains("fail-89\n"));

        let truncated = local_fallback(
            "awk 'BEGIN{for(i=0;i<200;i++) print i, \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}'",
            Path::new("/tmp"),
            "w1",
            5_000,
            "test",
            &[],
            None,
        );
        assert_eq!(truncated["ok"], true);
        assert_eq!(truncated["truncated"], true);
        assert!(truncated.get("compacted").is_none());
        assert!(
            truncated["output"]
                .as_str()
                .unwrap()
                .contains("…[truncated]")
        );
    }

    #[test]
    fn workspace_resolution_accepts_id_or_label() {
        let snapshot = json!({"workspaces": [{"workspace_id": "w1", "label": "demo"}]});
        assert_eq!(resolve_workspace(&snapshot, "w1").unwrap().id, "w1");
        assert_eq!(resolve_workspace(&snapshot, "demo").unwrap().id, "w1");
        assert!(resolve_workspace(&snapshot, "missing").is_none());
    }
}
